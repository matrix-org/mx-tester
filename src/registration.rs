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

use std::{collections::HashMap, convert::TryFrom};

use anyhow::{anyhow, Context, Error};
use async_trait::async_trait;
use data_encoding::HEXLOWER;
use hmac::{Hmac, Mac, NewMac};
use log::debug;
use matrix_sdk::ClientConfig;
use rand::Rng;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use typed_builder::TypedBuilder;

type HmacSha1 = Hmac<Sha1>;

/// The maximal number of attempts when registering a user..
const RETRY_ATTEMPTS: u64 = 10;

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
    use matrix_sdk::ruma::api::error::*;
    let homeserver_url = reqwest::Url::parse(base_url)?;
    let config = ClientConfig::new();
    config.get_request_config().retry_limit(RETRY_ATTEMPTS);
    let client = matrix_sdk::Client::new_with_config(homeserver_url, config)?;
    match client
        .login(&user.localname, &user.password, None, None)
        .await
    {
        Err(matrix_sdk::Error::Http(matrix_sdk::HttpError::ClientApi(
            FromHttpResponseError::Http(ServerError::Known(err)),
        ))) if err.kind == ErrorKind::Forbidden => {
            debug!("Could not authenticate {}", err);
            // Proceed with registration.
        }
        Ok(_) => return Ok(client),
        Err(err) => return Err(err).context("Error attempting to login"),
    }
    register_user(base_url, registration_shared_secret, user).await?;
    client
        .login(&user.localname, &user.password, None, None)
        .await?;
    Ok(client)
}

pub async fn handle_user_registration(config: &crate::Config) -> Result<(), Error> {
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
        clients.insert(user.localname.clone(), client);
    }
    // Create rooms
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
        let joined_rooms = client.joined_rooms();
        for room in &user.rooms {
            let mut request = matrix_sdk::ruma::api::client::r0::room::create_room::Request::new();
            if room.public {
                request.preset = Some(
                    matrix_sdk::ruma::api::client::r0::room::create_room::RoomPreset::PublicChat,
                );
            } else {
                request.preset = Some(
                    matrix_sdk::ruma::api::client::r0::room::create_room::RoomPreset::PrivateChat,
                );
            }
            if let Some(ref name) = room.name {
                request.name = Some(TryFrom::<&str>::try_from(name.as_str())?);
            }
            if let Some(ref alias) = room.alias {
                request.room_alias_name = Some(alias.as_ref());
                if joined_rooms.iter().any(|joined| {
                    matches!(joined.canonical_alias(), Some(joined_alias) if joined_alias.as_str() == alias)
                }) {
                    // Don't re-register the room.
                    continue;
                }
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
