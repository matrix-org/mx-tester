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
use data_encoding::HEXLOWER;
use hmac::{Hmac, Mac, NewMac};
use log::debug;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use sha1::Sha1;

type HmacSha1 = Hmac<Sha1>;

#[derive(Debug, Serialize)]
struct RegistrationPayload {
    nonce: String,
    username: String,
    displayname: String,
    password: String,
    admin: bool,
    mac: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RegistrationResponse {
    pub device_id: String,
    pub user_id: String,
    pub home_server: String,
    pub access_token: String,
}

#[derive(Debug, Deserialize)]
pub struct ErrorResponse {
    errcode: String,
    error: String,
}

#[derive(Debug, Deserialize)]
pub struct User {
    /// Create user as admin?
    #[serde(default)]
    pub admin: bool,

    pub localname: String,

    // if the password isn't provided, use the localname
    #[serde(default)]
    password: Option<String>,
}

impl User {
    pub fn password(&self) -> String {
        match &self.password {
            Some(password) => password.clone(),
            None => self.localname.clone(),
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
) -> Result<RegistrationResponse, Error> {
    #[derive(Debug, Deserialize)]
    struct GetRegisterResponse {
        nonce: String,
    }
    let registration_url = format!("{}/_synapse/admin/v1/register", base_url);
    debug!(
        "Registration shared secret: {}, url: {}, user: {:#?}",
        registration_shared_secret, registration_url, user
    );
    let nonce = reqwest::get(&registration_url)
        .await?
        .json::<GetRegisterResponse>()
        .await?
        .nonce;
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
            password = user.password(),
            admin = if user.admin { "admin" } else { "notadmin" }
        )
        .as_bytes(),
    );
    let registration_payload = RegistrationPayload {
        nonce,
        username: user.localname.to_string(),
        displayname: user.localname.to_string(),
        password: user.password(),
        admin: user.admin,
        mac: HEXLOWER.encode(&mac.finalize().into_bytes()),
    };
    debug!(
        "Sending payload {:#?}",
        serde_json::to_string_pretty(&registration_payload)
    );
    let client = reqwest::Client::new();
    let response = client
        .post(&registration_url)
        .json(&registration_payload)
        .send()
        .await?;
    match response.status() {
        StatusCode::OK => {
            let registration_info = response.json::<RegistrationResponse>().await?;
            Ok(registration_info)
        }
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

pub async fn login(base_url: &str, user: &User) -> Result<RegistrationResponse, Error> {
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
        password: user.password(),
        identifier: Identifier {
            identifier_type: "m.id.user".to_string(),
            user: user.localname.to_string(),
        },
    };
    let login_url = format!("{base_url}/_matrix/client/r0/login", base_url = base_url);
    let client = reqwest::Client::new();
    let response = client
        .post(login_url)
        .json(&login_payload)
        .send()
        .await?
        .json::<RegistrationResponse>()
        .await?;
    Ok(response)
}

/// Try to logion with the user details provided. If login fails, try to register that user.
/// If registration then fails, returns an error explaining why, otherwise returns the login details.
pub async fn ensure_user_exists(
    base_url: &str,
    registration_shared_secret: &str,
    user: &User,
) -> Result<RegistrationResponse, Error> {
    match login(base_url, user).await {
        Ok(response) => Ok(response),
        Err(_) => {
            debug!("Registering user {}", user.localname);
            Ok(register_user(base_url, registration_shared_secret, user).await?)
        }
    }
}
