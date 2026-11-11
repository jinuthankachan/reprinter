//! Optional admin password for the Settings page.
//!
//! With no password set, everyone is an admin (like the rest of a home LAN
//! print server). With one set, the settings API needs a session cookie from
//! `POST /api/login`. Sessions live in memory and end on restart.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use argon2::{
    Argon2,
    password_hash::{
        PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
        rand_core::{OsRng, RngCore},
    },
};
use axum::{
    extract::FromRequestParts,
    http::{HeaderMap, header, request::Parts},
};

use crate::{AppState, error::AppError};

pub const COOKIE: &str = "reprinter_session";
const SESSION_TTL: Duration = Duration::from_secs(12 * 3600);

#[derive(Default)]
pub struct Sessions(Mutex<HashMap<String, Instant>>);

impl Sessions {
    pub fn create(&self) -> String {
        let token = random_hex(32);
        let now = Instant::now();
        let mut map = self.0.lock().unwrap();
        map.retain(|_, expires| *expires > now);
        map.insert(token.clone(), now + SESSION_TTL);
        token
    }

    pub fn valid(&self, token: &str) -> bool {
        self.0
            .lock()
            .unwrap()
            .get(token)
            .is_some_and(|expires| *expires > Instant::now())
    }

    pub fn remove(&self, token: &str) {
        self.0.lock().unwrap().remove(token);
    }

    pub fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| {
        Argon2::default()
            .verify_password(password.as_bytes(), &h)
            .is_ok()
    })
}

/// The session token from the request's Cookie header(s).
pub fn session_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|c| {
            let (name, value) = c.trim().split_once('=')?;
            (name == COOKIE).then(|| value.to_string())
        })
}

pub fn set_cookie(token: &str) -> String {
    format!(
        "{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        SESSION_TTL.as_secs()
    )
}

pub fn clear_cookie() -> String {
    format!("{COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0")
}

/// Extractor for handlers that change settings: rejects the request with 401
/// unless no password is set or the request carries a valid session.
pub struct Admin;

impl FromRequestParts<Arc<AppState>> for Admin {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        if state.is_admin(&parts.headers) {
            Ok(Admin)
        } else {
            Err(AppError::unauthorized("log in to change the settings"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_hash_round_trip() {
        let hash = hash_password("hunter22").unwrap();
        assert!(verify_password("hunter22", &hash));
        assert!(!verify_password("hunter23", &hash));
        assert!(!verify_password("hunter22", "not a hash"));
    }

    #[test]
    fn finds_session_cookie_among_others() {
        let mut headers = HeaderMap::new();
        headers.append(
            header::COOKIE,
            "a=1; reprinter_session=abc".parse().unwrap(),
        );
        assert_eq!(session_token(&headers).as_deref(), Some("abc"));
        assert_eq!(session_token(&HeaderMap::new()), None);
    }

    #[test]
    fn sessions_expire_on_removal() {
        let s = Sessions::default();
        let t = s.create();
        assert!(s.valid(&t));
        s.remove(&t);
        assert!(!s.valid(&t));
    }
}
