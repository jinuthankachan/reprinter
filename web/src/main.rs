//! reprinter-web: a browser front end for a reprinter CUPS print server, and
//! a one-file installer for the whole print server.
//!
//! `reprinter-web install` sets up the host: it runs reprinter's installer
//! (install.sh, embedded in the binary) for CUPS, the printer driver and the
//! queue, then installs this binary as the reprinter-web service.
//!
//! `reprinter-web` (or `reprinter-web serve`) is the service: a single-page
//! interface (static/, embedded) and a JSON API under /api to print files,
//! watch, retry and cancel jobs, and change printer, CUPS and web interface
//! settings. In the background it cancels jobs that CUPS has held after a
//! printer failure for too long, so they do not clog the queue. It reads
//! state from the local scheduler over IPP and makes changes with the CUPS
//! command-line tools, so it runs as an unprivileged user in the `lpadmin`
//! group (see reprinter-web.service).

mod auth;
mod config;
mod cups;
mod embedded;
mod error;
mod ipp;
mod profiles;
mod routes;
mod setup;
mod uploads;

use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
    time::Duration,
};

use axum::http::HeaderMap;

use crate::config::Config;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
Usage:
  sudo reprinter-web install [options]   set up this host as a print server, with the web interface
  sudo reprinter-web uninstall [--purge] remove the web interface service
  reprinter-web [serve] [options]        run the web interface (what the service runs)

`reprinter-web install --help` lists the install options.

Options for serve:
  --config PATH       settings file (default /var/lib/reprinter/config.toml);
                      uploads are kept in an uploads/ directory next to it
  --listen ADDR       listen address, overriding the settings file (e.g. 127.0.0.1:8631)
  --share-dir PATH    read printer profiles from a reprinter checkout (its printers/ and
                      drivers/) instead of the ones built into this binary
  --cups-socket PATH  CUPS scheduler socket (default /run/cups/cups.sock)
  --clear-password    remove the admin password from the settings file and exit
  -V, --version       print the version and exit
  -h, --help          show this help
";

pub struct AppState {
    pub config: RwLock<Config>,
    pub config_path: PathBuf,
    /// The address actually listened on (config or --listen).
    pub listen: String,
    pub profiles: profiles::Source,
    /// Backends of reprinter's drivers (e.g. "captusb"), to tell reprinter's
    /// own queues from unwanted ones (`cups::flag_unwanted`).
    pub driver_schemes: Vec<String>,
    pub hostname: String,
    pub uploads: uploads::Uploads,
    pub sessions: auth::Sessions,
    pub login_gate: auth::LoginGate,
    pub ipp: ipp::Client,
}

impl AppState {
    pub fn config(&self) -> Config {
        self.config.read().unwrap().clone()
    }

    pub fn password_set(&self) -> bool {
        !self.config.read().unwrap().admin_password_hash.is_empty()
    }

    pub fn is_admin(&self, headers: &HeaderMap) -> bool {
        !self.password_set()
            || auth::session_token(headers).is_some_and(|t| self.sessions.valid(&t))
    }

    /// Applies a change to the settings and saves them; nothing changes in
    /// memory if saving fails.
    pub fn update_config(&self, change: impl FnOnce(&mut Config)) -> std::io::Result<()> {
        let mut current = self.config.write().unwrap();
        let mut next = current.clone();
        change(&mut next);
        next.clamp();
        next.save(&self.config_path)?;
        *current = next;
        Ok(())
    }
}

struct Args {
    config: PathBuf,
    share_dir: Option<PathBuf>,
    listen: Option<String>,
    cups_socket: PathBuf,
    clear_password: bool,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut args = Args {
        config: "/var/lib/reprinter/config.toml".into(),
        share_dir: None,
        listen: None,
        cups_socket: "/run/cups/cups.sock".into(),
        clear_password: false,
    };
    let mut argv = argv.iter();
    while let Some(arg) = argv.next() {
        let mut value = || {
            argv.next()
                .cloned()
                .ok_or_else(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--config" => args.config = value()?.into(),
            "--share-dir" => args.share_dir = Some(value()?.into()),
            "--listen" => args.listen = Some(value()?),
            "--cups-socket" => args.cups_socket = value()?.into(),
            "--clear-password" => args.clear_password = true,
            "-V" | "--version" => {
                println!("reprinter-web {VERSION}");
                std::process::exit(0);
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            _ => return Err(format!("unknown option: {arg}")),
        }
    }
    Ok(args)
}

fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("reprinter-web: {message}");
    std::process::exit(1);
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let result = match argv.first().map(String::as_str) {
        Some("install") => setup::install(&argv[1..]),
        Some("uninstall") => setup::uninstall(&argv[1..]),
        Some("serve") => serve_main(&argv[1..]),
        _ => serve_main(&argv),
    };
    if let Err(e) = result {
        fail(e);
    }
}

fn serve_main(argv: &[String]) -> Result<(), String> {
    let args = parse_args(argv).unwrap_or_else(|e| {
        eprintln!("reprinter-web: {e}\n\n{USAGE}");
        std::process::exit(2);
    });
    let config = Config::load(&args.config)?;

    if args.clear_password {
        let config = Config {
            admin_password_hash: String::new(),
            ..config
        };
        config
            .save(&args.config)
            .map_err(|e| format!("{}: {e}", args.config.display()))?;
        println!("Admin password removed from {}.", args.config.display());
        return Ok(());
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?
        .block_on(serve(args, config))
}

/// The user this process runs as, for IPP PeerCred authentication.
async fn current_user() -> String {
    let id = tokio::process::Command::new("id").arg("-un").output().await;
    match id {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => std::env::var("USER").unwrap_or_else(|_| "root".into()),
    }
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let term = async {
        match signal(SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term => {}
    }
    eprintln!("reprinter-web: shutting down");
}

async fn serve(args: Args, config: Config) -> Result<(), String> {
    let upload_dir = args
        .config
        .parent()
        .map(|d| d.join("uploads"))
        .unwrap_or_else(|| "uploads".into());
    let uploads = uploads::Uploads::new(upload_dir.clone())
        .map_err(|e| format!("{}: {e}", upload_dir.display()))?;
    let profiles = match args.share_dir {
        Some(dir) => {
            if !dir.join("printers").is_dir() {
                eprintln!(
                    "reprinter-web: warning: {}/printers does not exist",
                    dir.display()
                );
            }
            profiles::Source::Dir(dir)
        }
        None => profiles::Source::Embedded,
    };
    let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|h| h.trim().to_string())
        .unwrap_or_else(|_| "localhost".into());
    let listen = args.listen.clone().unwrap_or_else(|| config.listen.clone());
    let mut driver_schemes: Vec<String> = profiles::load_all(&profiles)
        .into_iter()
        .map(|p| p.uri_scheme)
        .filter(|s| !s.is_empty())
        .collect();
    driver_schemes.sort();
    driver_schemes.dedup();

    let state = Arc::new(AppState {
        config: RwLock::new(config),
        config_path: args.config,
        listen: listen.clone(),
        profiles,
        driver_schemes,
        hostname,
        uploads,
        sessions: auth::Sessions::default(),
        login_gate: auth::LoginGate::default(),
        ipp: ipp::Client::new(args.cups_socket, current_user().await),
    });

    // Expire unused uploads.
    let pruner = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            let ttl = Duration::from_secs(pruner.config().upload_ttl_minutes * 60);
            let n = pruner.uploads.prune(ttl);
            if n > 0 {
                eprintln!("uploads: removed {n} expired file(s)");
            }
        }
    });

    // Cancel jobs CUPS has held after a failure for longer than configured.
    let sweeper = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            let minutes = sweeper.config().stuck_job_minutes;
            if minutes == 0 {
                continue;
            }
            let age = Duration::from_secs(minutes * 60);
            match cups::cancel_jobs(&sweeper.ipp, None, true, age).await {
                Ok(ids) if !ids.is_empty() => eprintln!(
                    "jobs: canceled {} job(s) held after a printer failure for over {minutes} min: {ids:?}",
                    ids.len()
                ),
                Ok(_) => {}
                Err(e) => eprintln!("jobs: checking for stuck jobs: {e}"),
            }
        }
    });

    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .map_err(|e| format!("cannot listen on {listen}: {e}"))?;
    eprintln!(
        "reprinter-web {VERSION} listening on http://{listen}/ (uploads in {})",
        state.uploads.dir().display()
    );
    axum::serve(listener, routes::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| e.to_string())
}
