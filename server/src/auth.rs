use crate::{
    config::random_secret,
    error::ApiError,
    store::{ServerStore, SessionRecord, UserRecord, unix_to_rfc3339},
};
use anyhow::anyhow;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use axum::http::{HeaderMap, header};
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use rand::RngCore;
use sha2::{Digest, Sha256};

pub const SESSION_COOKIE: &str = "nuntius_session";

pub fn hash_secret(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

pub fn hash_password(password: &str) -> Result<String, ApiError> {
    if password.len() < 12 {
        return Err(ApiError::BadRequest(
            "password must contain at least 12 characters".into(),
        ));
    }
    let mut salt_bytes = [0_u8; 16];
    rand::rng().fill_bytes(&mut salt_bytes);
    let salt = SaltString::encode_b64(&salt_bytes)
        .map_err(|error| ApiError::internal(anyhow!(error.to_string())))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|v| v.to_string())
        .map_err(|error| ApiError::internal(anyhow!(error.to_string())))
}

pub fn verify_password(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash)
        .ok()
        .and_then(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .ok()
        })
        .is_some()
}

pub async fn create_session(
    store: &ServerStore,
    user: &UserRecord,
    ttl_hours: i64,
    user_agent: Option<&str>,
) -> Result<(String, String, String), ApiError> {
    let token = random_secret(32);
    let csrf = random_secret(24);
    let (_, expires_at) = store
        .create_session(
            user,
            &hash_secret(&token),
            &hash_secret(&csrf),
            ttl_hours,
            user_agent,
        )
        .await?;
    Ok((
        token,
        csrf,
        unix_to_rfc3339(expires_at).map_err(ApiError::internal)?,
    ))
}

pub async fn authenticate_web(
    store: &ServerStore,
    headers: &HeaderMap,
) -> Result<SessionRecord, ApiError> {
    let token = cookie_value(headers, SESSION_COOKIE).ok_or(ApiError::Unauthorized)?;
    let session = store
        .session_by_token_hash(&hash_secret(&token))
        .await?
        .ok_or(ApiError::Unauthorized)?;
    Ok(session)
}

pub async fn require_csrf(
    store: &ServerStore,
    headers: &HeaderMap,
    session: &SessionRecord,
) -> Result<(), ApiError> {
    let token = headers
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .ok_or(ApiError::Forbidden)?;
    if !store
        .csrf_token_valid(&session.id, &hash_secret(token))
        .await?
    {
        return Err(ApiError::Forbidden);
    }
    Ok(())
}

pub fn session_cookie(token: &str, secure: bool, max_age_seconds: i64) -> String {
    let secure_attribute = if secure { "; Secure" } else { "" };
    format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age_seconds}{secure_attribute}"
    )
}

pub fn clear_session_cookie(secure: bool) -> String {
    session_cookie("", secure, 0)
}

pub fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

pub fn verify_device_signature(
    public_key_b64: &str,
    nonce: &str,
    signature_b64: &str,
) -> Result<(), ApiError> {
    let public_bytes = base64::engine::general_purpose::STANDARD
        .decode(public_key_b64)
        .map_err(|_| ApiError::BadRequest("invalid device public key".into()))?;
    let public_array: [u8; 32] = public_bytes
        .try_into()
        .map_err(|_| ApiError::BadRequest("invalid device public key length".into()))?;
    let key = VerifyingKey::from_bytes(&public_array)
        .map_err(|_| ApiError::BadRequest("invalid device public key".into()))?;
    let signature_bytes = base64::engine::general_purpose::STANDARD
        .decode(signature_b64)
        .map_err(|_| ApiError::BadRequest("invalid signature".into()))?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|_| ApiError::BadRequest("invalid signature length".into()))?;
    key.verify(nonce.as_bytes(), &signature)
        .map_err(|_| ApiError::Forbidden)
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            (key == name).then(|| value.to_string())
        })
}
