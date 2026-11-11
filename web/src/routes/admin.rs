//! Settings: printers (queues), CUPS network sharing, the web interface
//! itself. Every handler takes the `Admin` extractor, so these need the admin
//! password once one is set.

use std::{collections::BTreeMap, net::SocketAddr, path::Path, sync::Arc};

use axum::{
    Json,
    extract::{Path as UrlPath, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::ok;
use crate::{
    AppState,
    auth::{self, Admin},
    config::{MAX_UPLOAD_MB, UPLOAD_TTL_MINUTES},
    cups::{self, Device, QUICK, SLOW, args},
    error::{AppError, Result},
    profiles::{self, Profile},
};

pub async fn settings(_: Admin, State(st): State<Arc<AppState>>) -> Result<Json<Value>> {
    let server = cups::server_settings().await?;
    let flag = |key: &str| server.get(key).is_some_and(|v| v == "1");
    let config = st.config();
    Ok(Json(json!({
        "cups": {
            "share_printers": flag("_share_printers"),
            "remote_any": flag("_remote_any"),
            "remote_admin": flag("_remote_admin"),
        },
        "web": {
            "listen": config.listen,
            "running_listen": st.listen,
            "max_upload_mb": config.max_upload_mb,
            "upload_ttl_minutes": config.upload_ttl_minutes,
            "password_set": !config.admin_password_hash.is_empty(),
        },
    })))
}

// ---------------------------------------------------------------------------
// CUPS server settings
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CupsUpdate {
    share_printers: bool,
    remote_any: bool,
    remote_admin: bool,
}

pub async fn set_cups(
    _: Admin,
    State(st): State<Arc<AppState>>,
    Json(u): Json<CupsUpdate>,
) -> Result<Json<Value>> {
    let flag = |on: bool, name: &str| {
        if on {
            format!("--{name}")
        } else {
            format!("--no-{name}")
        }
    };
    let cupsctl_args = [
        flag(u.share_printers, "share-printers"),
        flag(u.remote_any, "remote-any"),
        flag(u.remote_admin, "remote-admin"),
    ];
    // cupsctl uploads a new cupsd.conf, and the scheduler restarts itself.
    // Some CUPS builds (e.g. Ubuntu 24.04) only accept that upload from root,
    // whatever the user's groups: say what to run instead.
    if let Err(e) = cups::run("cupsctl", &cupsctl_args, QUICK).await {
        if e.message.contains("Unauthorized") || e.message.contains("Forbidden") {
            return Err(AppError::new(
                StatusCode::FORBIDDEN,
                format!(
                    "CUPS on this server only lets root change these settings. On the server, run: sudo cupsctl {}",
                    cupsctl_args.join(" ")
                ),
            ));
        }
        return Err(e);
    }
    // The restart happens just after cupsctl returns; wait it out.
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    cups::wait_ready(&st.ipp).await;
    eprintln!("settings: cupsctl {}", cupsctl_args.join(" "));
    Ok(ok())
}

// ---------------------------------------------------------------------------
// Web interface settings and password
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct WebUpdate {
    listen: String,
    max_upload_mb: u64,
    upload_ttl_minutes: u64,
}

pub async fn set_web(
    _: Admin,
    State(st): State<Arc<AppState>>,
    Json(u): Json<WebUpdate>,
) -> Result<Json<Value>> {
    let listen = u.listen.trim().to_string();
    if listen.parse::<SocketAddr>().is_err() {
        return Err(AppError::bad_request(
            "listen address must look like 0.0.0.0:8631 or [::]:8631",
        ));
    }
    if !MAX_UPLOAD_MB.contains(&u.max_upload_mb) {
        return Err(AppError::bad_request(format!(
            "upload limit must be {}–{} MB",
            MAX_UPLOAD_MB.start(),
            MAX_UPLOAD_MB.end()
        )));
    }
    if !UPLOAD_TTL_MINUTES.contains(&u.upload_ttl_minutes) {
        return Err(AppError::bad_request(format!(
            "upload lifetime must be {}–{} minutes",
            UPLOAD_TTL_MINUTES.start(),
            UPLOAD_TTL_MINUTES.end()
        )));
    }
    st.update_config(|c| {
        c.listen = listen.clone();
        c.max_upload_mb = u.max_upload_mb;
        c.upload_ttl_minutes = u.upload_ttl_minutes;
    })?;
    eprintln!("settings: web interface settings saved");
    Ok(Json(
        json!({ "ok": true, "restart_required": listen != st.listen }),
    ))
}

#[derive(Deserialize)]
pub struct PasswordUpdate {
    /// Empty removes the password.
    password: String,
}

pub async fn set_password(
    _: Admin,
    State(st): State<Arc<AppState>>,
    Json(u): Json<PasswordUpdate>,
) -> Result<Response> {
    let hash = if u.password.is_empty() {
        String::new()
    } else {
        if u.password.chars().count() < 6 {
            return Err(AppError::bad_request("use at least 6 characters"));
        }
        tokio::task::spawn_blocking(move || auth::hash_password(&u.password))
            .await
            .map_err(|e| AppError::internal(e.to_string()))?
            .map_err(AppError::internal)?
    };
    let set = !hash.is_empty();
    st.update_config(|c| c.admin_password_hash = hash)?;
    // Log everyone else out; keep the person who changed it logged in.
    st.sessions.clear();
    eprintln!(
        "settings: admin password {}",
        if set { "set" } else { "removed" }
    );
    if set {
        let token = st.sessions.create();
        Ok(([(header::SET_COOKIE, auth::set_cookie(&token))], ok()).into_response())
    } else {
        Ok(ok().into_response())
    }
}

// ---------------------------------------------------------------------------
// Printers (queues)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct PrinterUpdate {
    info: Option<String>,
    location: Option<String>,
    device_uri: Option<String>,
    shared: Option<bool>,
    enabled: Option<bool>,
    accepting: Option<bool>,
    #[serde(default)]
    make_default: bool,
    /// PPD option defaults, e.g. {"PageSize": "Letter"}.
    #[serde(default)]
    defaults: BTreeMap<String, String>,
}

pub async fn update_printer(
    _: Admin,
    State(st): State<Arc<AppState>>,
    UrlPath(name): UrlPath<String>,
    Json(u): Json<PrinterUpdate>,
) -> Result<Json<Value>> {
    let p = cups::find_printer(&st.ipp, &name).await?;

    let mut lpadmin = args(["-p", &p.name]);
    if let Some(info) = &u.info {
        lpadmin.extend(["-D".into(), cups::clean_text(info, 127)]);
    }
    if let Some(location) = &u.location {
        lpadmin.extend(["-L".into(), cups::clean_text(location, 127)]);
    }
    if let Some(uri) = u
        .device_uri
        .as_deref()
        .map(str::trim)
        .filter(|uri| *uri != p.device_uri)
    {
        if !cups::valid_device_uri(uri) {
            return Err(AppError::bad_request(format!("invalid device URI {uri}")));
        }
        lpadmin.extend(["-v".into(), uri.to_string()]);
    }
    if let Some(shared) = u.shared {
        lpadmin.extend(["-o".into(), format!("printer-is-shared={shared}")]);
    }
    if !u.defaults.is_empty() {
        cups::check_options(&u.defaults, &cups::ppd_options(&p.name).await?)?;
        for (key, value) in &u.defaults {
            lpadmin.extend(["-o".into(), format!("{key}={value}")]);
        }
    }
    if lpadmin.len() > 2 {
        cups::run("lpadmin", &lpadmin, SLOW).await?;
    }

    let enabled = p.state != "stopped";
    if let Some(on) = u.enabled.filter(|on| *on != enabled) {
        cups::run(
            if on { "cupsenable" } else { "cupsdisable" },
            std::slice::from_ref(&p.name),
            QUICK,
        )
        .await?;
    }
    if let Some(on) = u.accepting.filter(|on| *on != p.accepting) {
        cups::run(
            if on { "cupsaccept" } else { "cupsreject" },
            std::slice::from_ref(&p.name),
            QUICK,
        )
        .await?;
    }
    if u.make_default && !p.is_default {
        cups::run("lpadmin", &args(["-d", &p.name]), QUICK).await?;
    }
    eprintln!("settings: updated printer {}", p.name);
    Ok(ok())
}

pub async fn delete_printer(
    _: Admin,
    State(st): State<Arc<AppState>>,
    UrlPath(name): UrlPath<String>,
) -> Result<Json<Value>> {
    let p = cups::find_printer(&st.ipp, &name).await?;
    cups::run("lpadmin", &args(["-x", &p.name]), QUICK).await?;
    eprintln!("settings: deleted printer {}", p.name);
    Ok(ok())
}

pub async fn test_page(
    _: Admin,
    State(st): State<Arc<AppState>>,
    UrlPath(name): UrlPath<String>,
) -> Result<Json<Value>> {
    let p = cups::find_printer(&st.ipp, &name).await?;
    let page = [
        "/usr/share/cups/data/testprint",
        "/usr/local/share/cups/data/testprint",
    ]
    .into_iter()
    .find(|f| Path::new(f).exists())
    .ok_or_else(|| {
        AppError::internal("the CUPS test page (cups/data/testprint) is not installed")
    })?;
    let job_id = cups::submit(&args(["-d", &p.name, "-t", "Test page", page]), None).await?;
    Ok(Json(json!({ "job_id": job_id })))
}

/// Devices on the same backend as the printer's current URI, to fix a
/// changed URI (e.g. after the printer moved to another USB port).
pub async fn printer_devices(
    _: Admin,
    State(st): State<Arc<AppState>>,
    UrlPath(name): UrlPath<String>,
) -> Result<Json<Vec<Device>>> {
    let p = cups::find_printer(&st.ipp, &name).await?;
    let scheme = p.device_uri.split(':').next().unwrap_or_default();
    let mut list = cups::devices(scheme).await?;
    for d in &mut list {
        d.matches = d.uri == p.device_uri;
    }
    Ok(Json(list))
}

// ---------------------------------------------------------------------------
// Adding printers from reprinter's profiles
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct ProfileInfo {
    #[serde(flatten)]
    profile: Profile,
    connected: bool,
    driver_installed: bool,
    queue_exists: bool,
}

pub async fn profiles(_: Admin, State(st): State<Arc<AppState>>) -> Result<Json<Value>> {
    let list = profiles::load_all(&st.share_dir);
    let usb = profiles::usb_devices();
    let models = if list.is_empty() {
        Default::default()
    } else {
        cups::models().await.unwrap_or_else(|e| {
            eprintln!("lpinfo -m: {e}");
            Default::default()
        })
    };
    let printers = cups::printers(&st.ipp).await?;
    let list: Vec<ProfileInfo> = list
        .into_iter()
        .map(|profile| ProfileInfo {
            connected: profile.usb_ids.iter().any(|id| usb.contains(id)),
            driver_installed: profile.ppd == "everywhere" || models.contains(&profile.ppd),
            queue_exists: printers.iter().any(|q| q.name == profile.queue),
            profile,
        })
        .collect();
    Ok(Json(
        json!({ "share_dir": st.share_dir.display().to_string(), "profiles": list }),
    ))
}

fn find_profile(st: &AppState, id: &str) -> Result<Profile> {
    profiles::find(&st.share_dir, id)
        .ok_or_else(|| AppError::not_found(format!("no printer profile {id}")))
}

/// Devices on the profile's backend; those matching its URI pattern first.
pub async fn profile_devices(
    _: Admin,
    State(st): State<Arc<AppState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Vec<Device>>> {
    let profile = find_profile(&st, &id)?;
    if profile.uri_scheme.is_empty() {
        return Err(AppError::internal(format!(
            "drivers/{}.sh sets no DRIVER_URI_SCHEME",
            profile.driver
        )));
    }
    let pattern = regex::Regex::new(&profile.uri_regex).ok();
    let mut list = cups::devices(&profile.uri_scheme).await?;
    for d in &mut list {
        d.matches = pattern.as_ref().is_some_and(|re| re.is_match(&d.uri));
    }
    list.sort_by_key(|d| !d.matches);
    Ok(Json(list))
}

#[derive(Deserialize)]
pub struct NewPrinter {
    profile: String,
    name: String,
    device_uri: String,
    paper: String,
    #[serde(default)]
    shared: bool,
}

/// Creates (or updates in place) a queue from a profile, the same way
/// install.sh does. The driver must already be installed.
pub async fn create_printer(
    _: Admin,
    State(st): State<Arc<AppState>>,
    Json(n): Json<NewPrinter>,
) -> Result<Json<Value>> {
    let profile = find_profile(&st, &n.profile)?;
    let name = n.name.trim();
    if !cups::valid_queue_name(name) {
        return Err(AppError::bad_request(
            "the queue name may only contain letters, digits, '_', '.' and '-'",
        ));
    }
    let uri = n.device_uri.trim();
    if !cups::valid_device_uri(uri) {
        return Err(AppError::bad_request(format!("invalid device URI {uri}")));
    }
    let paper = n.paper.trim();
    if paper.is_empty()
        || paper.len() > 64
        || !paper
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
    {
        return Err(AppError::bad_request("invalid paper size"));
    }
    if profile.ppd != "everywhere" && !cups::models().await?.contains(&profile.ppd) {
        return Err(AppError::bad_request(format!(
            "the {} driver is not installed. On the server, run: ./install.sh --printer {}",
            profile.driver, profile.id
        )));
    }

    let description = format!("{} ({})", profile.model, profile.driver);
    let mut lpadmin = args([
        "-p",
        name,
        "-E",
        "-v",
        uri,
        "-m",
        &profile.ppd,
        "-L",
        &st.hostname,
        "-D",
        &description,
    ]);
    lpadmin.extend(["-o".into(), format!("printer-is-shared={}", n.shared)]);
    lpadmin.extend(["-o".into(), format!("media={paper}")]);
    for option in &profile.options {
        lpadmin.extend(["-o".into(), option.clone()]);
    }
    cups::run("lpadmin", &lpadmin, SLOW).await?;
    cups::run("cupsenable", &[name.to_string()], QUICK).await?;
    cups::run("cupsaccept", &[name.to_string()], QUICK).await?;
    if cups::default_printer(&st.ipp).await.is_none() {
        cups::run("lpadmin", &args(["-d", name]), QUICK).await?;
    }
    eprintln!("settings: created printer {name} ({}, {uri})", profile.id);
    Ok(ok())
}
