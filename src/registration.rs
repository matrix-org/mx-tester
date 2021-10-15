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

use anyhow::{anyhow, Error};
use async_trait::async_trait;
use data_encoding::HEXLOWER;
use hmac::{Hmac, Mac, NewMac};
use log::debug;
use rand::Rng;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use typed_builder::TypedBuilder;

type HmacSha1 = Hmac<Sha1>;

const ATTEMPTS: u64 = 10;
const INTERVAL: std::ops::Range<u64> = 300..1000;

#[derive(Clone, TypedBuilder, Debug, Default, Deserialize)]
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
}

impl User {
    fn default_password() -> String {
        "password".to_string()
    }
}

#[async_trait]
trait Retry {
    async fn auto_retry(&self, attempts: u64) -> Result<reqwest::Response, Error>;
}

#[async_trait]
impl Retry for reqwest::RequestBuilder {
    async fn auto_retry(&self, max_attempts: u64) -> Result<reqwest::Response, Error> {
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
                        let duration = (attempt * attempt) * rand::thread_rng().gen_range(INTERVAL);
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
pub async fn register_user(
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
        .auto_retry(ATTEMPTS)
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
        .auto_retry(ATTEMPTS)
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

pub async fn login(base_url: &str, user: &User) -> Result<(), Error> {
    #[derive(Debug, Serialize)]
    struct Identifier {
        #[serde(rename(serialize = "type"))]
        identifier_type: String,
        user: String,
    }
    #[derive(Debug, Serialize)]
    struct LoginPayload {
        #[serde(rename(serialize = "type"))]
        login_type: String,
        identifier: Identifier,
        password: String,
    }
    let login_payload = LoginPayload {
        login_type: "m.login.password".to_string(),
        password: user.password.to_string(),
        identifier: Identifier {
            identifier_type: "m.id.user".to_string(),
            user: user.localname.to_string(),
        },
    };
    let login_url = format!("{base_url}/_matrix/client/r0/login", base_url = base_url);
    let response = reqwest::Client::new()
        .post(&login_url)
        .json(&login_payload)
        .auto_retry(ATTEMPTS)
        .await?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(anyhow!("Login error: {:?}", response.text().await))
    }
}

/// Try to login with the user details provided. If login fails, try to register that user.
/// If registration then fails, returns an error explaining why, otherwise returns the login details.
pub async fn ensure_user_exists(
    base_url: &str,
    registration_shared_secret: &str,
    user: &User,
) -> Result<(), Error> {
    debug!("ensure_user_exists {}", base_url);
    match login(base_url, user).await {
        Ok(response) => Ok(response),
        Err(err) => {
            debug!("Registering user {} {}", user.localname, err);
            Ok(register_user(base_url, registration_shared_secret, user).await?)
        }
    }
}
