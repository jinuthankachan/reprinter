//! Admin password for the Settings page, and the checks that keep other web
//! sites out.
//!
//! `reprinter-web install` sets a random password when there is none. With a
//! password set, the settings API needs a session cookie from
//! `POST /api/login`. Sessions live in memory and end on restart. Without one
//! (removed on purpose, or a server started by hand), everyone is an admin.

use std::{
    collections::HashMap,
    net::{Ipv4Addr, Ipv6Addr},
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
use tokio::sync::Semaphore;

use crate::{AppState, error::AppError};

pub const COOKIE: &str = "reprinter_session";
const SESSION_TTL: Duration = Duration::from_secs(12 * 3600);
/// How long a login waits for its turn before giving up (see `LoginGate`).
const LOGIN_WAIT: Duration = Duration::from_secs(10);
/// How long a wrong password holds up the next login.
const LOGIN_PENALTY: Duration = Duration::from_secs(1);

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

/// A random password for the installer to set: 16 lowercase letters and
/// digits without look-alikes (about 79 bits), in groups of four so it is
/// easy to copy from the terminal, e.g. "k7pq-x3mz-r9tw-b2hc".
pub fn generate_password() -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    // Bytes from the top, incomplete round of the alphabet are drawn again,
    // so that every character is equally likely.
    let fair = 256 - 256 % ALPHABET.len();
    let mut out = String::with_capacity(19);
    let mut count = 0;
    while count < 16 {
        let mut byte = [0u8; 1];
        OsRng.fill_bytes(&mut byte);
        let b = byte[0] as usize;
        if b >= fair {
            continue;
        }
        if count > 0 && count % 4 == 0 {
            out.push('-');
        }
        out.push(ALPHABET[b % ALPHABET.len()] as char);
        count += 1;
    }
    out
}

/// Password checks run one at a time, and a wrong password keeps the next
/// check waiting for a second. An Argon2 check needs about 19 MiB, so many
/// logins at once could run a small board out of memory; this also limits
/// guessing to about one try per second, for all clients together.
///
/// The turn is held by the blocking task itself, so a client that hangs up
/// early neither frees it sooner nor leaves a check running outside of it.
pub struct LoginGate(Arc<Semaphore>);

impl Default for LoginGate {
    fn default() -> Self {
        Self(Arc::new(Semaphore::new(1)))
    }
}

impl LoginGate {
    /// Whether `password` matches `hash`; None when no turn came up within
    /// `LOGIN_WAIT` (someone is sending many logins).
    pub async fn check(&self, password: String, hash: String) -> Option<bool> {
        let turn = tokio::time::timeout(LOGIN_WAIT, self.0.clone().acquire_owned())
            .await
            .ok()?
            .ok()?;
        let ok = tokio::task::spawn_blocking(move || {
            let ok = verify_password(&password, &hash);
            if !ok {
                std::thread::sleep(LOGIN_PENALTY);
            }
            drop(turn);
            ok
        })
        .await
        .unwrap_or(false);
        Some(ok)
    }
}

/// Guard against DNS rebinding: whether the page may answer a request whose
/// `Host` header is `host`.
///
/// A web site can point its own domain name at this server's address. The
/// browser then treats this server as part of that site, so the site's
/// scripts can call the API, custom headers and all. The browser still sends
/// the site's domain in `Host`, so answering only to names this server really
/// has stops that: any IP address (a page served from an address is from
/// this server), localhost, the host name, HOSTNAME.local, and the names in
/// `extra` (`allowed_hosts` in the settings).
pub fn host_allowed(host: &str, hostname: &str, extra: &[String]) -> bool {
    let host = host.trim();
    if let Some(rest) = host.strip_prefix('[') {
        // [v6]:port
        return rest
            .split_once(']')
            .is_some_and(|(ip, _)| ip.parse::<Ipv6Addr>().is_ok());
    }
    let name = host
        .split_once(':')
        .map_or(host, |(name, _)| name)
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if name.is_empty() {
        return false;
    }
    if name == "localhost" || name.parse::<Ipv4Addr>().is_ok() {
        return true;
    }
    let hostname = hostname.trim_end_matches('.').to_ascii_lowercase();
    let short = hostname.split('.').next().unwrap_or_default();
    let own = [
        hostname.clone(),
        format!("{hostname}.local"),
        short.to_string(),
        format!("{short}.local"),
    ];
    (!hostname.is_empty() && own.contains(&name))
        || extra
            .iter()
            .any(|e| e.trim().trim_end_matches('.').eq_ignore_ascii_case(&name))
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
    fn generated_passwords() {
        let a = generate_password();
        assert_eq!(a.len(), 19);
        assert_eq!(a.matches('-').count(), 3);
        assert!(a.split('-').all(|g| g.len() == 4));
        assert!(
            a.chars()
                .all(|c| c == '-' || "abcdefghjkmnpqrstuvwxyz23456789".contains(c))
        );
        assert_ne!(a, generate_password());
    }

    #[tokio::test]
    async fn login_gate_checks_and_slows_down_wrong_passwords() {
        let hash = hash_password("right-one").unwrap();
        let gate = LoginGate::default();
        assert_eq!(
            gate.check("right-one".into(), hash.clone()).await,
            Some(true)
        );
        let start = Instant::now();
        assert_eq!(gate.check("wrong".into(), hash.clone()).await, Some(false));
        assert!(start.elapsed() >= LOGIN_PENALTY);
    }

    #[test]
    fn hosts() {
        let extra = vec!["Printer.LAN".to_string()];
        let ok = |h: &str| host_allowed(h, "printpi", &extra);
        for h in [
            "10.0.0.5",
            "10.0.0.5:8631",
            "[::1]:8631",
            "[fe80::1]",
            "localhost:8631",
            "printpi",
            "PrintPi.local:8631",
            "printpi.local.",
            "printer.lan",
            "printer.lan:80",
        ] {
            assert!(ok(h), "{h}");
        }
        for h in [
            "",
            "evil.example",
            "evil.example:8631",
            "printpi.evil.example",
            "10.0.0.5.evil.example",
            "localhost.evil.example",
            "[evil]",
            "[::1",
            "other.local",
        ] {
            assert!(!ok(h), "{h}");
        }
        // A fully qualified host name also answers to its short name.
        assert!(host_allowed("printpi.local", "printpi.home.arpa", &[]));
        assert!(host_allowed("printpi.home.arpa", "printpi.home.arpa", &[]));
        assert!(!host_allowed(".local", "", &[]));
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
