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

use serde::{Serialize, Deserialize};
use sha1::Sha1;
use data_encoding::HEXLOWER;
use hmac::{Hmac, Mac, NewMac};
use log::debug;


type HmacSha1 = Hmac<Sha1>;

#[derive(Debug, Deserialize)]
struct GetRegisterResponse {
    nonce: String,
}

#[derive(Debug, Serialize)]
struct RegistrationPayload {
    nonce: String,
    username: String,
    displayname: String,
    password: String,
    admin: bool,
    mac: String,
}

#[derive(Debug, Deserialize)]
pub struct RegistrationResponse {
    pub device_id: String,
    pub user_id: String,
    pub home_server: String,
    pub access_token: String,
}

pub async fn register_user(
    base_url: &str,
    registaration_shared_secret: String,
    username: String,
    password: String,
    displayname: String,
    is_admin: bool,
) -> Result<RegistrationResponse, reqwest::Error> {
    let registration_url = format!("{}/_synapse/admin/v1/register", base_url);
    let client = reqwest::Client::new();
    let nonce = reqwest::get(&registration_url)
        .await?
        .json::<GetRegisterResponse>()
        .await?
        .nonce;
    let mut mac = HmacSha1::new_from_slice(registaration_shared_secret.as_bytes())
        .expect("Couldn't use the provided registration shared secret to createa a hmac");
    mac.update(
        format!(
            "{nonce}\0{username}\0{password}\0{admin}",
            nonce = nonce,
            username = username,
            password = password,
            admin = if is_admin { "admin" } else { "nodadmin" }
        )
        .as_bytes(),
    );
    let registration_payload = RegistrationPayload {
        nonce,
        username,
        displayname,
        password,
        admin: is_admin,
        mac: HEXLOWER.encode(&mac.finalize().into_bytes()),
    };
    debug!(
        "Sending payload {:#?}",
        serde_json::to_string_pretty(&registration_payload)
    );
    let response = client
        .post(&registration_url)
        .json(&registration_payload)
        .send()
        .await?
        .json::<RegistrationResponse>()
        .await?;
    debug!("Responded with {:#?}", response);
    Ok(response)
}
