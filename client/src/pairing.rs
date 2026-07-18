use crate::{
    config::{self, ClientConfig},
    protocol::*,
};
use anyhow::{Context, Result, bail};
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use reqwest::{Client, Response, redirect};
use std::fs;
use std::time::Duration;

pub fn signing_key() -> Result<SigningKey> {
    let encoded = fs::read_to_string(config::device_key_path()?)
        .context("device key is missing; run `nuntius-client init`")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .context("invalid device key")?;
    let array: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid device key length"))?;
    Ok(SigningKey::from_bytes(&array))
}

pub async fn pair(config: &mut ClientConfig, code: &str) -> Result<String> {
    config.validate()?;
    if config.device_id.is_some() {
        bail!(
            "this client is already paired; remove device_id from config.toml only when intentionally repairing pairing"
        )
    };
    let key = signing_key()?;
    let request = PairDeviceRequest {
        code: code.trim().to_uppercase(),
        display_name: config.display_name.clone(),
        public_key: base64::engine::general_purpose::STANDARD
            .encode(key.verifying_key().as_bytes()),
        agent_version: env!("CARGO_PKG_VERSION").into(),
        os_family: std::env::consts::OS.into(),
        architecture: std::env::consts::ARCH.into(),
    };
    let response = checked(
        http_client()?
            .post(endpoint(config, "api/v1/device-auth/pair")?)
            .json(&request)
            .send()
            .await?,
    )
    .await?;
    let paired: PairDeviceResponse = response.json().await?;
    config.device_id = Some(paired.device_id.clone());
    config.save()?;
    Ok(paired.device_id)
}

pub async fn access_token(config: &ClientConfig) -> Result<String> {
    let device_id = config.device_id.as_ref().context("client is not paired")?;
    let client = http_client()?;
    let response = checked(
        client
            .post(endpoint(config, "api/v1/device-auth/challenge")?)
            .json(&ChallengeRequest {
                device_id: device_id.clone(),
            })
            .send()
            .await?,
    )
    .await?;
    let challenge: ChallengeResponse = response.json().await?;
    let signature = signing_key()?.sign(challenge.nonce.as_bytes());
    let token = checked(
        client
            .post(endpoint(config, "api/v1/device-auth/token")?)
            .json(&DeviceTokenRequest {
                device_id: device_id.clone(),
                challenge_id: challenge.challenge_id,
                signature: base64::engine::general_purpose::STANDARD.encode(signature.to_bytes()),
            })
            .send()
            .await?,
    )
    .await?
    .json::<DeviceTokenResponse>()
    .await?;
    Ok(token.access_token)
}

pub fn endpoint(config: &ClientConfig, path: &str) -> Result<url::Url> {
    Ok(url::Url::parse(&config.server_url)?.join(path.trim_start_matches('/'))?)
}
async fn checked(response: Response) -> Result<Response> {
    let status = response.status();
    if status.is_success() {
        Ok(response)
    } else {
        bail!("server returned {status}")
    }
}

fn http_client() -> Result<Client> {
    Ok(Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        // Pairing codes and device signatures must never be forwarded by an HTTP redirect.
        .redirect(redirect::Policy::none())
        .build()?)
}
