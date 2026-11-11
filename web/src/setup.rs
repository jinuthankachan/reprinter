//! `reprinter-web install` and `reprinter-web uninstall`: set up a print
//! server from this one file.
//!
//! install runs reprinter's own installer, embedded in the binary (install.sh
//! with lib/, drivers/ and printers/), for CUPS, the printer driver and the
//! queue. Then it installs this binary as the reprinter-web systemd service:
//! service user, settings file, unit, firewall. Both parts are safe to rerun.

use std::{
    net::{SocketAddr, TcpStream},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use crate::{auth, config::Config, embedded};

const BIN: &str = "/usr/local/bin/reprinter-web";
const UNIT_PATH: &str = "/etc/systemd/system/reprinter-web.service";
const STATE_DIR: &str = "/var/lib/reprinter";
const CONFIG: &str = "/var/lib/reprinter/config.toml";
const USER: &str = "reprinter";
const DEFAULT_PORT: u16 = 8631;
const UNIT: &str = include_str!("../reprinter-web.service");

const INSTALL_USAGE: &str = "\
Usage: sudo reprinter-web install [options]

Sets up this host as a print server: CUPS, the driver and a shared queue for
the printer on USB (reprinter's install.sh, built into this binary), then the
web interface as the reprinter-web service. Safe to rerun.

Printer (passed to install.sh):
  -p, --printer ID    printer profile (default: auto-detect on USB); see --list
  -q, --queue NAME    CUPS queue name (default: the profile's, e.g. LBP1210)
      --paper SIZE    default paper size (default: a4; e.g. letter)
      --uri URI       use this device URI instead of searching for the printer
      --from-source   build the driver from source instead of using a prebuilt package
  -l, --list          list supported printers, and whether each is connected, and exit

Web interface:
      --port PORT     port to listen on (default: 8631, or the one already configured)
      --web-only      skip the printer setup; only install the web interface

  --local             printer not shared on the network, web interface on 127.0.0.1 only
  -h, --help          show this help
";

const UNINSTALL_USAGE: &str = "\
Usage: sudo reprinter-web uninstall [--purge]

Stops and removes the reprinter-web service and /usr/local/bin/reprinter-web.
--purge also deletes the settings (/var/lib/reprinter) and the reprinter user.
Printers, CUPS and drivers are left alone (see the README to remove them).
";

fn step(message: &str) {
    println!("\n==> {message}");
}

fn info(message: &str) {
    println!("    {message}");
}

fn warn(message: &str) {
    eprintln!("WARNING: {message}");
}

/// Runs a command quietly; on failure the error carries its output.
fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let detail = [stderr.trim(), stdout.trim()]
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or("");
        Err(format!("{program} {} failed: {detail}", args.join(" ")))
    }
}

fn succeeds(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn have(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .chain(["/usr/sbin".into(), "/sbin".into()])
        .any(|dir: PathBuf| dir.join(program).is_file())
}

fn require_root(what: &str) -> Result<(), String> {
    let uid = run("id", &["-u"])?;
    if uid.trim() == "0" {
        Ok(())
    } else {
        Err(format!("{what} needs root: run it with sudo"))
    }
}

fn systemd() -> bool {
    Path::new("/run/systemd/system").is_dir()
}

struct Options {
    /// Arguments for install.sh.
    printer: Vec<String>,
    port: Option<u16>,
    local: bool,
    web_only: bool,
    list: bool,
}

fn parse(argv: &[String]) -> Result<Options, String> {
    let mut o = Options {
        printer: Vec::new(),
        port: None,
        local: false,
        web_only: false,
        list: false,
    };
    let mut argv = argv.iter();
    while let Some(arg) = argv.next() {
        let mut value = || {
            argv.next()
                .cloned()
                .ok_or_else(|| format!("{arg} needs a value (see --help)"))
        };
        match arg.as_str() {
            "-p" | "--printer" | "-q" | "--queue" | "--paper" | "--uri" => {
                let v = value()?;
                o.printer.extend([arg.clone(), v]);
            }
            "--from-source" => o.printer.push(arg.clone()),
            "--local" => {
                o.local = true;
                o.printer.push(arg.clone());
            }
            "-l" | "--list" => o.list = true,
            "--port" => {
                let v = value()?;
                o.port = Some(
                    v.parse()
                        .ok()
                        .filter(|p| *p > 0)
                        .ok_or_else(|| format!("invalid port: {v}"))?,
                );
            }
            "--web-only" => o.web_only = true,
            "-h" | "--help" => {
                print!("{INSTALL_USAGE}");
                std::process::exit(0);
            }
            _ => {
                return Err(format!(
                    "unknown option: {arg} (see reprinter-web install --help)"
                ));
            }
        }
    }
    Ok(o)
}

/// The embedded installer, extracted into a private temporary directory that
/// is removed again when dropped.
struct Installer(PathBuf);

impl Installer {
    fn extract() -> Result<Self, String> {
        let dir = std::env::temp_dir().join(format!("reprinter-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        let installer = Installer(dir);
        embedded::extract(&installer.0).map_err(|e| format!("extracting the installer: {e}"))?;
        Ok(installer)
    }

    fn run(&self, args: &[String]) -> Result<(), String> {
        let status = Command::new("bash")
            .arg(self.0.join("install.sh"))
            .args(args)
            .status()
            .map_err(|e| format!("cannot run bash: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err("the printer setup failed (see above)".into())
        }
    }
}

impl Drop for Installer {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn install(argv: &[String]) -> Result<(), String> {
    let o = parse(argv)?;
    if o.list {
        return Installer::extract()?.run(&["--list".to_string()]);
    }
    require_root("reprinter-web install")?;

    if o.web_only {
        if !have("lpstat") {
            return Err(
                "CUPS is not installed. Run reprinter-web install without --web-only to set it up."
                    .into(),
            );
        }
    } else {
        step("Printer: CUPS, driver and queue (reprinter install.sh)");
        Installer::extract()?.run(&o.printer)?;
    }

    step("Web interface");
    install_binary()?;
    let (listen, password) = install_settings(&o)?;
    install_service(&listen)?;
    open_firewall(&listen);
    report(&listen, password.as_deref());
    Ok(())
}

/// Copies this executable to /usr/local/bin, replacing a running one safely
/// (write a new file, then rename over the old one).
fn install_binary() -> Result<(), String> {
    let me = std::env::current_exe().map_err(|e| format!("cannot find this program: {e}"))?;
    let target = Path::new(BIN);
    if me.canonicalize().ok() == target.canonicalize().ok() {
        info(&format!("{BIN} is this binary"));
        return Ok(());
    }
    let tmp = target.with_extension("new");
    let io = |e: std::io::Error| format!("installing {BIN}: {e}");
    std::fs::create_dir_all(target.parent().unwrap()).map_err(io)?;
    std::fs::copy(&me, &tmp).map_err(io)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(io)?;
    std::fs::rename(&tmp, target).map_err(io)?;
    info(&format!("Installed {BIN}"));
    Ok(())
}

/// Service user, state directory and settings file. Returns the listen
/// address, and the admin password when one was generated.
fn install_settings(o: &Options) -> Result<(String, Option<String>), String> {
    if !succeeds("id", &[USER]) {
        info(&format!("Creating the {USER} system user"));
        run(
            "useradd",
            &[
                "--system",
                "--user-group",
                "--home-dir",
                STATE_DIR,
                "--no-create-home",
                "--shell",
                "/usr/sbin/nologin",
                USER,
            ],
        )?;
    }
    // lpadmin membership is what CUPS checks before letting it change printers.
    run("usermod", &["-aG", "lpadmin", USER])?;

    std::fs::create_dir_all(STATE_DIR).map_err(|e| format!("{STATE_DIR}: {e}"))?;
    std::fs::set_permissions(STATE_DIR, std::fs::Permissions::from_mode(0o750))
        .map_err(|e| format!("{STATE_DIR}: {e}"))?;

    let path = Path::new(CONFIG);
    let existed = path.exists();
    let mut config = Config::load(path)?;
    let mut changed = false;
    if !existed || o.port.is_some() || o.local {
        let port = o
            .port
            .unwrap_or_else(|| port_of(&config.listen).unwrap_or(DEFAULT_PORT));
        config.listen = format!("{}:{port}", if o.local { "127.0.0.1" } else { "0.0.0.0" });
        changed = true;
    }
    // Without a password, anyone who can open the page could change the
    // settings, e.g. point a printer at their own server and get every
    // document printed on it. So the settings start out locked.
    let password = if config.admin_password_hash.is_empty() {
        let password = auth::generate_password();
        config.admin_password_hash = auth::hash_password(&password)?;
        changed = true;
        Some(password)
    } else {
        None
    };
    if changed {
        config.save(path).map_err(|e| format!("{CONFIG}: {e}"))?;
        info(&format!("Settings: {CONFIG} (listen {})", config.listen));
    } else {
        info(&format!(
            "Keeping the settings in {CONFIG} (listen {})",
            config.listen
        ));
    }
    if password.is_some() {
        info("Set a new admin password for the Settings page (shown at the end)");
    }
    run("chown", &["-R", &format!("{USER}:{USER}"), STATE_DIR])?;
    Ok((config.listen, password))
}

fn port_of(listen: &str) -> Option<u16> {
    listen.parse::<SocketAddr>().ok().map(|a| a.port())
}

fn install_service(listen: &str) -> Result<(), String> {
    std::fs::create_dir_all(Path::new(UNIT_PATH).parent().unwrap())
        .map_err(|e| format!("{UNIT_PATH}: {e}"))?;
    std::fs::write(UNIT_PATH, UNIT).map_err(|e| format!("{UNIT_PATH}: {e}"))?;
    std::fs::set_permissions(UNIT_PATH, std::fs::Permissions::from_mode(0o644))
        .map_err(|e| format!("{UNIT_PATH}: {e}"))?;
    if !systemd() {
        warn("no systemd here: start the web interface by hand:");
        warn(&format!(
            "  sudo runuser -u {USER} -- {BIN} --config {CONFIG}"
        ));
        return Ok(());
    }
    info("Starting the reprinter-web service");
    run("systemctl", &["daemon-reload"])?;
    run("systemctl", &["enable", "reprinter-web"])?;
    run("systemctl", &["restart", "reprinter-web"])?;
    let port = port_of(listen).unwrap_or(DEFAULT_PORT);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    for _ in 0..40 {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    warn("reprinter-web did not come up; check: journalctl -u reprinter-web");
    Ok(())
}

fn local_only(listen: &str) -> bool {
    listen
        .parse::<SocketAddr>()
        .is_ok_and(|a| a.ip().is_loopback())
}

/// Opens the port when ufw or firewalld is active, like install.sh does for IPP.
fn open_firewall(listen: &str) {
    if local_only(listen) {
        return;
    }
    let Some(port) = port_of(listen) else { return };
    let rule = format!("{port}/tcp");
    if have("ufw") && run("ufw", &["status"]).is_ok_and(|s| s.contains("Status: active")) {
        info(&format!("ufw is active: allowing {rule}"));
        if let Err(e) = run("ufw", &["allow", &rule, "comment", "reprinter web"]) {
            warn(&e);
        }
    } else if have("firewall-cmd") && succeeds("firewall-cmd", &["--state"]) {
        info(&format!("firewalld is running: allowing {rule}"));
        if let Err(e) = run(
            "firewall-cmd",
            &["--permanent", &format!("--add-port={rule}")],
        )
        .and_then(|_| run("firewall-cmd", &["--reload"]))
        {
            warn(&e);
        }
    }
}

fn report(listen: &str, password: Option<&str>) {
    let port = port_of(listen).unwrap_or(DEFAULT_PORT);
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|h| h.trim().to_string())
        .unwrap_or_default();
    let ip = run("hostname", &["-I"])
        .ok()
        .and_then(|o| o.split_whitespace().next().map(str::to_string));
    step("Done");
    if local_only(listen) {
        println!("Web interface: http://127.0.0.1:{port}/   (this host only)");
    } else {
        println!(
            "Web interface: http://{}:{port}/   (also http://{host}.local:{port}/)",
            ip.as_deref().unwrap_or("<this-host-ip>")
        );
    }
    match password {
        Some(password) => println!(
            "\nAdmin password for the Settings page: {password}\n\
             It is not shown again; change it under Settings -> Web interface."
        ),
        None => println!("\nThe Settings page keeps its admin password."),
    }
    println!(
        "Anyone who can open the page can print; changing the settings needs the password.\n\n\
         Logs:      journalctl -u reprinter-web\n\
         Settings:  {CONFIG}\n\
         Forgot the admin password? Remove it, and the next install sets a new one:\n  \
           sudo -u {USER} {BIN} --config {CONFIG} --clear-password\n  \
           sudo {BIN} install --web-only"
    );
}

pub fn uninstall(argv: &[String]) -> Result<(), String> {
    let mut purge = false;
    for arg in argv {
        match arg.as_str() {
            "--purge" => purge = true,
            "-h" | "--help" => {
                print!("{UNINSTALL_USAGE}");
                return Ok(());
            }
            _ => {
                return Err(format!(
                    "unknown option: {arg} (see reprinter-web uninstall --help)"
                ));
            }
        }
    }
    require_root("reprinter-web uninstall")?;
    step("Removing the reprinter-web service");
    if systemd() {
        let _ = run("systemctl", &["disable", "--now", "reprinter-web"]);
    }
    for path in [UNIT_PATH, BIN] {
        match std::fs::remove_file(path) {
            Ok(()) => info(&format!("Removed {path}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn(&format!("{path}: {e}")),
        }
    }
    if systemd() {
        let _ = run("systemctl", &["daemon-reload"]);
    }
    if purge {
        let _ = std::fs::remove_dir_all(STATE_DIR);
        info(&format!("Removed {STATE_DIR}"));
        if succeeds("id", &[USER]) {
            run("userdel", &[USER])?;
            info(&format!("Removed the {USER} user"));
        }
    } else {
        info(&format!(
            "Kept the settings in {STATE_DIR} and the {USER} user (--purge removes them)."
        ));
    }
    info("Printers, CUPS and drivers are unchanged; the README's Uninstall section removes them.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn printer_options_go_to_install_sh() {
        let o = parse(&args(&[
            "--printer",
            "canon-lbp1210",
            "-q",
            "Laser",
            "--paper",
            "letter",
            "--from-source",
            "--port",
            "80",
        ]))
        .unwrap();
        assert_eq!(
            o.printer,
            args(&[
                "--printer",
                "canon-lbp1210",
                "-q",
                "Laser",
                "--paper",
                "letter",
                "--from-source"
            ])
        );
        assert_eq!(o.port, Some(80));
        assert!(!o.local && !o.web_only);
    }

    #[test]
    fn local_applies_to_both() {
        let o = parse(&args(&["--local", "--web-only"])).unwrap();
        assert!(o.local && o.web_only);
        assert_eq!(o.printer, args(&["--local"]));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse(&args(&["--port", "0"])).is_err());
        assert!(parse(&args(&["--port", "http"])).is_err());
        assert!(parse(&args(&["--printer"])).is_err());
        assert!(parse(&args(&["--bogus"])).is_err());
    }

    #[test]
    fn listen_helpers() {
        assert_eq!(port_of("0.0.0.0:8631"), Some(8631));
        assert_eq!(port_of("[::]:80"), Some(80));
        assert!(local_only("127.0.0.1:8631"));
        assert!(!local_only("0.0.0.0:8631"));
    }

    #[test]
    fn unit_runs_the_installed_binary() {
        assert!(UNIT.contains(&format!("ExecStart={BIN} --config {CONFIG}")));
        assert!(UNIT.contains(&format!("User={USER}")));
    }
}
