// Copyright 2021 The Matrix.org Foundation C.I.C.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::{
    collections::{HashMap, HashSet},
    convert::TryFrom,
};

use anyhow::{anyhow, Context, Error};
use async_trait::async_trait;
use data_encoding::HEXLOWER;
use hmac::{Hmac, Mac};
use log::debug;
use matrix_sdk::{
    ruma::{api::client::error::ErrorKind, RoomAliasId},
    HttpError,
};
use rand::Rng;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use typed_builder::TypedBuilder;

use crate::util::AsRumaError;

type HmacSha1 = Hmac<Sha1>;

/// The maximal number of attempts when registering a user..
const RETRY_ATTEMPTS: u64 = 10;
const TIMEOUT_SEC: u64 = 15;

#[derive(Clone, Debug, Deserialize)]
pub enum RateLimit {
    /// Leave the rate limit unchanged.
    #[serde(alias = "default")]
    Default,

    /// Specify that the user shouldn't be rate-limited.
    #[serde(alias = "unlimited")]
    Unlimited,
}
impl Default for RateLimit {
    fn default() -> Self {
        RateLimit::Default
    }
}

#[derive(Clone, TypedBuilder, Debug, Deserialize)]
pub struct User {
    /// Create user as admin?
    #[serde(default)]
    #[builder(default = false)]
    pub admin: bool,

    pub localname: String,

    /// The password for this user. If unspecified, we use `"password"` as password.
    #[serde(default = "User::default_password")]
    #[builder(default = User::default_password())]
    pub password: String,

    #[serde(default)]
    #[builder(default)]
    pub rooms: Vec<Room>,

    /// If specified, override the maximal number of messages per second
    /// that this user can send.
    #[serde(default)]
    #[builder(default)]
    pub rate_limit: RateLimit,
}

impl User {
    fn default_password() -> String {
        "password".to_string()
    }
}

/// Instructions for creating a room.
#[derive(Clone, TypedBuilder, Debug, Deserialize)]
pub struct Room {
    /// Whether the room should be public.
    #[serde(default)]
    #[builder(default = false)]
    pub public: bool,

    /// A list of room members.
    ///
    /// These must have been created by mx-tester.
    #[serde(default)]
    #[builder(default)]
    pub members: Vec<String>,

    /// A name for the room.
    #[serde(default)]
    #[builder(default)]
    pub name: Option<String>,

    /// A public alias for the room.
    #[serde(default)]
    #[builder(default)]
    pub alias: Option<String>,

    /// A topic for the room.
    #[serde(default)]
    #[builder(default)]
    pub topic: Option<String>,
}

#[async_trait]
trait Retry {
    async fn auto_retry(&self, attempts: u64) -> Result<reqwest::Response, Error>;
}

#[async_trait]
impl Retry for reqwest::RequestBuilder {
    async fn auto_retry(&self, max_attempts: u64) -> Result<reqwest::Response, Error> {
        /// The duration of the retry will be picked randomly within this interval,
        /// plus an exponential backoff.
        const BASE_INTERVAL_MS: std::ops::Range<u64> = 300..1000;

        let mut attempt = 1;
        loop {
            match self
                .try_clone()
                .expect("Cannot auto-retry non-clonable requests")
                .send()
                .await
            {
                Ok(response) => {
                    debug!("auto_retry success");
                    break Ok(response);
                }
                Err(err) => {
                    debug!("auto_retry error {:?} => {:?}", err, err.status());
                    // FIXME: Is this the right way to decide when to retry?
                    let should_retry = attempt < max_attempts
                        && (err.is_connect() || err.is_timeout() || err.is_request());

                    if should_retry {
                        let duration =
                            (attempt * attempt) * rand::thread_rng().gen_range(BASE_INTERVAL_MS);
                        attempt += 1;
                        debug!("auto_retry: sleeping {}ms", duration);
                        tokio::time::sleep(std::time::Duration::from_millis(duration)).await;
                    } else {
                        debug!("auto_retry: giving up!");
                        return Err(err.into());
                    }
                }
            }
        }
    }
}

/// Register a user using the admin api and a registration shared secret.
/// The base_url is the Scheme and Authority of the URL to access synapse via.
/// Returns a RegistrationResponse if registration succeeded, otherwise returns an error.
async fn register_user(
    base_url: &str,
    registration_shared_secret: &str,
    user: &User,
) -> Result<(), Error> {
    #[derive(Debug, Deserialize)]
    struct GetRegisterResponse {
        nonce: String,
    }
    let registration_url = format!("{}/_synapse/admin/v1/register", base_url);
    debug!(
        "Registration shared secret: {}, url: {}, user: {:#?}",
        registration_shared_secret, registration_url, user
    );
    let client = reqwest::Client::new();
    let nonce = client
        .get(&registration_url)
        .auto_retry(RETRY_ATTEMPTS)
        .await?
        .json::<GetRegisterResponse>()
        .await?
        .nonce;
    // We use map_err here because Hmac::InvalidKeyLength doesn't implement the std::error::Error trait.
    let mut mac =
        HmacSha1::new_from_slice(registration_shared_secret.as_bytes()).map_err(|err| {
            anyhow!(
                "Couldn't use the provided registration shared secret to create a hmac: {}",
                err
            )
        })?;
    mac.update(
        format!(
            "{nonce}\0{username}\0{password}\0{admin}",
            nonce = nonce,
            username = user.localname,
            password = user.password,
            admin = if user.admin { "admin" } else { "notadmin" }
        )
        .as_bytes(),
    );

    #[derive(Debug, Serialize)]
    struct RegistrationPayload {
        nonce: String,
        username: String,
        displayname: String,
        password: String,
        admin: bool,
        mac: String,
    }

    let registration_payload = RegistrationPayload {
        nonce,
        username: user.localname.to_string(),
        displayname: user.localname.to_string(),
        password: user.password.to_string(),
        admin: user.admin,
        mac: HEXLOWER.encode(&mac.finalize().into_bytes()),
    };
    debug!(
        "Sending payload {:#?}",
        serde_json::to_string_pretty(&registration_payload)
    );

    #[derive(Debug, Deserialize)]
    struct ErrorResponse {
        errcode: String,
        error: String,
    }
    let client = reqwest::Client::new();
    let response = client
        .post(&registration_url)
        .json(&registration_payload)
        .auto_retry(RETRY_ATTEMPTS)
        .await?;
    match response.status() {
        StatusCode::OK => Ok(()),
        _ => {
            let body = response.json::<ErrorResponse>().await?;
            Err(anyhow!(
                "Homeserver responded with errcode: {}, error: {}",
                body.errcode,
                body.error
            ))
        }
    }
}

/// Try to login with the user details provided. If login fails, try to register that user.
/// If registration then fails, returns an error explaining why, otherwise returns the login details.
async fn ensure_user_exists(
    base_url: &str,
    registration_shared_secret: &str,
    user: &User,
) -> Result<matrix_sdk::Client, Error> {
    debug!(
        "ensure_user_exists at {}: user {} with password {}",
        base_url, user.localname, user.password
    );
    use matrix_sdk::ruma::api::client::error::*;
    let homeserver_url = reqwest::Url::parse(base_url)?;
    let request_config = matrix_sdk::config::RequestConfig::new()
        .retry_limit(RETRY_ATTEMPTS)
        .retry_timeout(std::time::Duration::new(TIMEOUT_SEC, 0));
    let client = matrix_sdk::Client::builder()
        .request_config(request_config)
        .homeserver_url(homeserver_url)
        .build()
        .await?;
    match client
        .login(&user.localname, &user.password, None, None)
        .await
    {
        Ok(_) => return Ok(client),
        Err(err) => {
            match err.as_ruma_error() {
                Some(err) if err.kind == ErrorKind::Forbidden => {
                    debug!("Could not authenticate {}", err);
                    // Proceed with registration.
                }
                _ => return Err(err).context("Error attempting to login"),
            }
        }
    }
    register_user(base_url, registration_shared_secret, user).await?;
    client
        .login(&user.localname, &user.password, None, None)
        .await?;
    Ok(client)
}

pub async fn handle_user_registration(config: &crate::Config) -> Result<(), Error> {
    // Create an admin user. We'll need it later to unthrottle users.
    let admin = ensure_user_exists(
        &config.homeserver.public_baseurl,
        &config.homeserver.registration_shared_secret,
        &User::builder()
            .admin(true)
            .localname("mx-tester-admin".to_string())
            .build(),
    )
    .await?;

    let mut clients = HashMap::new();
    // Create users
    for user in &config.users {
        let client = ensure_user_exists(
            &config.homeserver.public_baseurl,
            &config.homeserver.registration_shared_secret,
            user,
        )
        .await
        .with_context(|| format!("Could not setup user {}", user.localname))?;

        // If the user is not rate limited, remove the rate limit.
        if let RateLimit::Unlimited = user.rate_limit {
            use override_rate_limits::*;
            let user_id = client
                .user_id()
                .await
                .expect("Client doesn't have a user id");
            let request = Request::new(&user_id, Some(0), Some(0));
            let _ = admin.send(request, None).await?;
        }

        clients.insert(user.localname.clone(), client);
    }

    // Create rooms
    let mut aliases = HashSet::new();
    for user in &config.users {
        if user.rooms.is_empty() {
            continue;
        }
        let client = clients.get(&user.localname).unwrap(); // We just inserted it.
        let my_user_id = client.user_id().await.ok_or_else(|| {
            anyhow!(
                "Cannot determine full user id for own user {}.",
                user.localname
            )
        })?;
        for room in &user.rooms {
            let mut request = matrix_sdk::ruma::api::client::room::create_room::v3::Request::new();
            if room.public {
                request.preset = Some(
                    matrix_sdk::ruma::api::client::room::create_room::v3::RoomPreset::PublicChat,
                );
            } else {
                request.preset = Some(
                    matrix_sdk::ruma::api::client::room::create_room::v3::RoomPreset::PrivateChat,
                );
            }
            if let Some(ref name) = room.name {
                request.name = Some(TryFrom::<&str>::try_from(name.as_str())?);
            }
            if let Some(ref alias) = room.alias {
                if !aliases.insert(alias) {
                    return Err(anyhow!(
                        "Attempting to create more than one room with alias {}",
                        alias
                    ));
                }
                request.room_alias_name = Some(alias.as_ref());
                // If the alias is already taken, we may need to remove it.
                let full_alias = format!("#{}:{}", alias, config.homeserver.server_name);
                debug!("Attempting to register alias {}, this may require unregistering previous instances first.", full_alias);
                let room_alias_id = <&RoomAliasId as TryFrom<&str>>::try_from(full_alias.as_ref())?;
                match client
                    .send(
                        matrix_sdk::ruma::api::client::alias::delete_alias::v3::Request::new(
                            &room_alias_id,
                        ),
                        None,
                    )
                    .await
                {
                    // Room alias was successfully removed.
                    Ok(_) => Ok(()),
                    // Room alias wasn't removed because it didn't exist.
                    Err(HttpError::Server(ref code)) if code.as_u16() == 404 => Ok(()),
                    Err(err) => {
                        match err.as_ruma_error() {
                            Some(err) if err.kind == ErrorKind::NotFound => Ok(()),
                            // Room alias wasn't removed for any other reason.
                            _ => Err(err),
                        }
                    }
                }
                .context("Error while attempting to unregister existing alias")?;
            }
            if let Some(ref topic) = room.topic {
                request.topic = Some(topic.as_ref());
            }

            // Place invites.
            let mut invites = vec![];
            for member in &room.members {
                let member_client = clients.get(member).ok_or_else(|| {
                    anyhow!(
                        "Cannot invite user {}: we haven't created this user.",
                        member
                    )
                })?;
                let user_id = member_client
                    .user_id()
                    .await
                    .ok_or_else(|| anyhow!("Cannot determine full user id for user {}.", member))?;
                if my_user_id == user_id {
                    // Don't invite oneself.
                    continue;
                }
                invites.push(user_id);
            }
            request.invite = &invites;
            let room_id = client.create_room(request).await?.room_id;

            // Respond to invites.
            for member in &room.members {
                let member_client = clients.get(member).unwrap(); // We checked this a few lines ago.
                member_client.join_room_by_id(&room_id).await?;
            }
        }
    }
    Ok(())
}

mod override_rate_limits {
    use matrix_sdk::ruma::api::ruma_api;
    use matrix_sdk::ruma::UserId;
    use serde::{Deserialize, Serialize};

    ruma_api! {
        metadata: {
            description: "Override rate limits",
            method: POST,
            name: "override_rate_limit",
            unstable_path: "/_synapse/admin/v1/users/:user_id/override_ratelimit",
            rate_limited: false,
            authentication: AccessToken,
        }

        request: {
            /// user ID
            #[ruma_api(path)]
            pub user_id: &'a UserId,

            /// The number of actions that can be performed in a second. Defaults to 0.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub messages_per_second: Option<u32>,

            /// How many actions that can be performed before being limited. Defaults to 0.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub burst_count: Option<u32>
        }

        response: {
            /// Details about the user.
            #[ruma_api(body)]
            pub limits: UserLimits,
        }
    }

    #[derive(Serialize, Deserialize, Clone, Debug)]
    pub struct UserLimits {
        pub messages_per_second: u32,
        pub burst_count: u32,
    }

    impl<'a> Request<'a> {
        /// Creates an `Request` with the given user ID.
        pub fn new(
            user_id: &'a UserId,
            messages_per_second: Option<u32>,
            burst_count: Option<u32>,
        ) -> Self {
            Self {
                user_id,
                messages_per_second,
                burst_count,
            }
        }
    }

    impl Response {
        /// Creates a new `Response` with all parameters defaulted.
        #[allow(dead_code)]
        pub fn new(limits: UserLimits) -> Self {
            Self { limits }
        }
    }
}
