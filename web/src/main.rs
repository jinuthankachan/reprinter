//! reprinter-web: a browser front end for a reprinter CUPS print server.
//!
//! Serves a single-page interface (static/, embedded in the binary) and a
//! JSON API under /api: upload a file, choose the page setup and print it;
//! watch and cancel jobs; change printer, CUPS and web interface settings.
//! It reads state from the local scheduler over IPP and makes changes with
//! the CUPS command-line tools, so it runs as an unprivileged user in the
//! `lpadmin` group (see reprinter-web.service).

mod auth;
mod config;
mod cups;
mod error;
mod ipp;
mod profiles;
mod routes;
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
Usage: reprinter-web [options]

  --config PATH       settings file (default /var/lib/reprinter/config.toml);
                      uploads are kept in an uploads/ directory next to it
  --share-dir PATH    directory with reprinter's printers/ and drivers/
                      (default /usr/local/share/reprinter)
  --listen ADDR       listen address, overriding the settings file (e.g. 127.0.0.1:8631)
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
    pub share_dir: PathBuf,
    pub hostname: String,
    pub uploads: uploads::Uploads,
    pub sessions: auth::Sessions,
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
    share_dir: PathBuf,
    listen: Option<String>,
    cups_socket: PathBuf,
    clear_password: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        config: "/var/lib/reprinter/config.toml".into(),
        share_dir: "/usr/local/share/reprinter".into(),
        listen: None,
        cups_socket: "/run/cups/cups.sock".into(),
        clear_password: false,
    };
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        let mut value = || argv.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--config" => args.config = value()?.into(),
            "--share-dir" => args.share_dir = value()?.into(),
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

#[tokio::main]
async fn main() {
    let args = parse_args().unwrap_or_else(|e| {
        eprintln!("reprinter-web: {e}\n\n{USAGE}");
        std::process::exit(2);
    });
    let config = Config::load(&args.config).unwrap_or_else(|e| fail(e));

    if args.clear_password {
        let config = Config {
            admin_password_hash: String::new(),
            ..config
        };
        config
            .save(&args.config)
            .unwrap_or_else(|e| fail(format!("{}: {e}", args.config.display())));
        println!("Admin password removed from {}.", args.config.display());
        return;
    }

    let upload_dir = args
        .config
        .parent()
        .map(|d| d.join("uploads"))
        .unwrap_or_else(|| "uploads".into());
    let uploads = uploads::Uploads::new(upload_dir.clone())
        .unwrap_or_else(|e| fail(format!("{}: {e}", upload_dir.display())));
    if !args.share_dir.join("printers").is_dir() {
        eprintln!(
            "reprinter-web: warning: no printer profiles in {}/printers; \"Add a printer\" will be empty (see --share-dir)",
            args.share_dir.display()
        );
    }
    let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|h| h.trim().to_string())
        .unwrap_or_else(|_| "localhost".into());
    let listen = args.listen.clone().unwrap_or_else(|| config.listen.clone());

    let state = Arc::new(AppState {
        config: RwLock::new(config),
        config_path: args.config,
        listen: listen.clone(),
        share_dir: args.share_dir,
        hostname,
        uploads,
        sessions: auth::Sessions::default(),
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

    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .unwrap_or_else(|e| fail(format!("cannot listen on {listen}: {e}")));
    eprintln!(
        "reprinter-web {VERSION} listening on http://{listen}/ (uploads in {})",
        state.uploads.dir().display()
    );
    axum::serve(listener, routes::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap_or_else(|e| fail(e));
}
