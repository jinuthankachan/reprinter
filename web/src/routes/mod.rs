//! HTTP routes: the embedded web page and the JSON API.

mod admin;
mod print;

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    AppState, VERSION, auth,
    error::{AppError, Result},
};

const INDEX_HTML: &str = include_str!("../../static/index.html");
const APP_JS: &str = include_str!("../../static/app.js");
const STYLE_CSS: &str = include_str!("../../static/style.css");
const FAVICON_SVG: &str = include_str!("../../static/favicon.svg");

const PAGE_CSP: &str = "default-src 'self'; img-src 'self' data: blob:; frame-src 'self'; \
                        object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'self'";

pub fn router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .route("/session", get(session))
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/printers", get(print::printers))
        .route("/printers/{name}/options", get(print::options))
        // The upload handler enforces the configurable size limit itself.
        .route(
            "/uploads",
            post(print::upload).layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/uploads/{id}",
            get(print::upload_file).delete(print::upload_delete),
        )
        .route("/print", post(print::print))
        .route("/jobs", get(print::jobs).delete(print::cancel_many))
        .route("/jobs/{id}", axum::routing::delete(print::cancel))
        .route("/jobs/{id}/retry", post(print::retry))
        .route("/admin/settings", get(admin::settings))
        .route("/admin/cups", put(admin::set_cups))
        .route("/admin/web", put(admin::set_web))
        .route("/admin/password", put(admin::set_password))
        .route("/admin/printers", post(admin::create_printer))
        .route(
            "/admin/printers/{name}",
            put(admin::update_printer).delete(admin::delete_printer),
        )
        .route("/admin/printers/{name}/test-page", post(admin::test_page))
        .route(
            "/admin/printers/{name}/devices",
            get(admin::printer_devices),
        )
        .route("/admin/profiles", get(admin::profiles))
        .route("/admin/profiles/{id}/devices", get(admin::profile_devices))
        .fallback(|| async { AppError::not_found("no such API endpoint") })
        .layer(middleware::from_fn(require_api_header));

    Router::new()
        .route("/", get(|| async { page() }))
        .route(
            "/app.js",
            get(|| async { asset("text/javascript; charset=utf-8", APP_JS) }),
        )
        .route(
            "/style.css",
            get(|| async { asset("text/css; charset=utf-8", STYLE_CSS) }),
        )
        .route(
            "/favicon.svg",
            get(|| async { asset("image/svg+xml", FAVICON_SVG) }),
        )
        .nest("/api", api)
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}

fn page() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
            (header::CONTENT_SECURITY_POLICY, PAGE_CSP),
        ],
        INDEX_HTML,
    )
        .into_response()
}

fn asset(content_type: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

/// Cross-site request forgery guard. Any web page can make a visitor's
/// browser POST a form to this server, but it cannot add a custom header
/// without a CORS preflight, which this server never approves. So every
/// request that changes something must carry `X-Reprinter`.
async fn require_api_header(request: Request, next: Next) -> Response {
    let safe = matches!(*request.method(), Method::GET | Method::HEAD);
    if !safe && !request.headers().contains_key("x-reprinter") {
        return AppError::new(StatusCode::FORBIDDEN, "missing X-Reprinter header").into_response();
    }
    next.run(request).await
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let h = response.headers_mut();
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.entry(header::X_FRAME_OPTIONS)
        .or_insert(HeaderValue::from_static("SAMEORIGIN"));
    response
}

#[derive(Serialize)]
struct SessionInfo {
    hostname: String,
    version: &'static str,
    password_set: bool,
    admin: bool,
    max_upload_mb: u64,
}

async fn session(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Json<SessionInfo> {
    Json(SessionInfo {
        hostname: st.hostname.clone(),
        version: VERSION,
        password_set: st.password_set(),
        admin: st.is_admin(&headers),
        max_upload_mb: st.config().max_upload_mb,
    })
}

#[derive(Deserialize)]
struct Login {
    password: String,
}

async fn login(State(st): State<Arc<AppState>>, Json(body): Json<Login>) -> Result<Response> {
    let hash = st.config().admin_password_hash;
    if hash.is_empty() {
        return Ok(Json(json!({ "ok": true })).into_response());
    }
    let ok = tokio::task::spawn_blocking(move || auth::verify_password(&body.password, &hash))
        .await
        .unwrap_or(false);
    if !ok {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await; // slow down guessing
        return Err(AppError::unauthorized("wrong password"));
    }
    let token = st.sessions.create();
    Ok((
        [(header::SET_COOKIE, auth::set_cookie(&token))],
        Json(json!({ "ok": true })),
    )
        .into_response())
}

async fn logout(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(token) = auth::session_token(&headers) {
        st.sessions.remove(&token);
    }
    (
        [(header::SET_COOKIE, auth::clear_cookie())],
        Json(json!({ "ok": true })),
    )
        .into_response()
}

fn ok() -> Json<Value> {
    Json(json!({ "ok": true }))
}
