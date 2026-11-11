//! Everything the web interface asks of CUPS.
//!
//! State (printers, jobs, the default printer) is read over IPP (`ipp.rs`).
//! Changes use the same command-line tools as `install.sh` (`lp`, `lpadmin`,
//! `cupsenable`, `cupsctl`, ...), run in the C locale with arguments passed
//! directly (never through a shell). Every value that reaches them is
//! validated here first.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    process::Stdio,
    time::Duration,
};

use serde::Serialize;

use crate::{
    error::{AppError, Result},
    ipp,
};

pub const QUICK: Duration = Duration::from_secs(30);
/// lpinfo probes backends; lp may upload a large file.
pub const SLOW: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------------
// Command-line tools
// ---------------------------------------------------------------------------

/// Runs a CUPS tool and returns its standard output. A non-zero exit becomes
/// an error carrying the tool's own message.
pub async fn run(program: &str, args: &[String], limit: Duration) -> Result<String> {
    run_with_input(program, args, limit, None).await
}

/// Like `run`, with the tool's standard input read from `input`.
pub async fn run_with_input(
    program: &str,
    args: &[String],
    limit: Duration,
    input: Option<&Path>,
) -> Result<String> {
    let stdin = match input {
        Some(path) => Stdio::from(std::fs::File::open(path)?),
        None => Stdio::null(),
    };
    let mut cmd = tokio::process::Command::new(locate(program));
    cmd.args(args)
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env_remove("LPDEST")
        .env_remove("PRINTER")
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = match tokio::time::timeout(limit, cmd.output()).await {
        Err(_) => {
            return Err(AppError::cups(format!(
                "{program} did not finish within {} s",
                limit.as_secs()
            )));
        }
        Ok(Err(e)) => return Err(AppError::internal(format!("cannot run {program}: {e}"))),
        Ok(Ok(out)) => out,
    };
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let msg = [stderr.trim(), stdout.trim()]
            .into_iter()
            .find(|m| !m.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{program} failed ({})", out.status));
        return Err(AppError::cups(msg));
    }
    Ok(stdout)
}

/// lpadmin and lpinfo live in /usr/sbin, which is not on every user's PATH.
fn locate(program: &str) -> String {
    ["/usr/sbin", "/usr/bin", "/sbin", "/bin"]
        .iter()
        .map(|dir| format!("{dir}/{program}"))
        .find(|p| Path::new(p).exists())
        .unwrap_or_else(|| program.to_string())
}

pub fn args<const N: usize>(list: [&str; N]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Queue names as install.sh accepts them: letters, digits, `_ . -`.
pub fn valid_queue_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 127
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
}

pub fn valid_scheme(scheme: &str) -> bool {
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "+.-".contains(c))
}

pub fn valid_device_uri(uri: &str) -> bool {
    uri.len() <= 1024
        && !uri.chars().any(|c| c.is_whitespace() || c.is_control())
        && uri
            .split_once(':')
            .is_some_and(|(scheme, rest)| valid_scheme(scheme) && !rest.is_empty())
}

/// Free text for descriptions and job titles: no control characters, bounded.
pub fn clean_text(text: &str, max_chars: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Normalises a page list such as "1-3, 5, 4" into "1-5". The scheduler only
/// accepts ascending, non-overlapping ranges, so ranges are sorted and merged.
/// Empty means all pages.
pub fn normalize_page_ranges(text: &str) -> Result<Option<String>, String> {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty() {
        return Ok(None);
    }
    let bad = || {
        format!(
            "invalid page list \"{}\": use pages and ranges like 1-3, 5",
            text.trim()
        )
    };
    let mut ranges = Vec::new();
    for part in compact.split(',') {
        let (a, b) = part.split_once('-').unwrap_or((part, part));
        let a: u32 = a.parse().map_err(|_| bad())?;
        let b: u32 = b.parse().map_err(|_| bad())?;
        if a == 0 || b < a {
            return Err(bad());
        }
        ranges.push((a, b));
    }
    ranges.sort();
    let mut merged: Vec<(u32, u32)> = Vec::new();
    for (a, b) in ranges {
        match merged.last_mut() {
            Some(last) if a <= last.1.saturating_add(1) => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    let parts: Vec<String> = merged
        .iter()
        .map(|&(a, b)| {
            if a == b {
                a.to_string()
            } else {
                format!("{a}-{b}")
            }
        })
        .collect();
    Ok(Some(parts.join(",")))
}

// ---------------------------------------------------------------------------
// Printers and jobs (IPP)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Printer {
    pub name: String,
    pub info: String,
    pub location: String,
    pub make_model: String,
    /// "idle", "printing" or "stopped"
    pub state: &'static str,
    pub state_message: String,
    pub state_reasons: Vec<String>,
    pub accepting: bool,
    pub shared: bool,
    pub is_default: bool,
    pub device_uri: String,
    pub queued_jobs: i32,
    /// Why this queue only gets in the way and should be removed; see
    /// `flag_unwanted`. Not set by `printers`.
    pub unwanted: Option<String>,
}

const PRINTER_ATTRS: &[&str] = &[
    "printer-name",
    "printer-info",
    "printer-location",
    "printer-make-and-model",
    "printer-state",
    "printer-state-message",
    "printer-state-reasons",
    "printer-is-accepting-jobs",
    "printer-is-shared",
    "printer-is-temporary",
    "printer-type",
    "device-uri",
    "queued-job-count",
];

/// printer-type bits for queues that live on another server.
const CUPS_PRINTER_REMOTE: i32 = 0x0002;
const CUPS_PRINTER_DISCOVERED: i32 = 0x0100_0000;

/// The local queues, sorted by name. Remote and auto-created temporary
/// queues are left out.
pub async fn printers(ipp: &ipp::Client) -> Result<Vec<Printer>> {
    let request =
        ipp::Request::new(ipp::CUPS_GET_PRINTERS).keywords("requested-attributes", PRINTER_ATTRS);
    let response = ipp.call("/", request).await.map_err(AppError::cups)?;
    if response.status == ipp::STATUS_NOT_FOUND {
        return Ok(Vec::new()); // "No destinations added."
    }
    if !response.ok() {
        return Err(AppError::cups(response.message()));
    }
    let default = default_printer(ipp).await;
    let mut list: Vec<Printer> = response
        .groups(ipp::GROUP_PRINTER)
        .filter(|g| g.bool("printer-is-temporary") != Some(true))
        .filter(|g| {
            g.int("printer-type").unwrap_or(0) & (CUPS_PRINTER_REMOTE | CUPS_PRINTER_DISCOVERED)
                == 0
        })
        .filter_map(|g| {
            let name = g.str("printer-name")?.to_string();
            Some(Printer {
                is_default: default.as_deref() == Some(name.as_str()),
                info: g.string("printer-info"),
                location: g.string("printer-location"),
                make_model: g.string("printer-make-and-model"),
                state: match g.int("printer-state") {
                    Some(4) => "printing",
                    Some(5) => "stopped",
                    _ => "idle",
                },
                state_message: g.string("printer-state-message"),
                state_reasons: g
                    .strings("printer-state-reasons")
                    .into_iter()
                    .filter(|r| r != "none")
                    .collect(),
                accepting: g.bool("printer-is-accepting-jobs").unwrap_or(false),
                shared: g.bool("printer-is-shared").unwrap_or(false),
                device_uri: g.string("device-uri"),
                queued_jobs: g.int("queued-job-count").unwrap_or(0),
                unwanted: None,
                name,
            })
        })
        .collect();
    list.sort_by_key(|p| p.name.to_lowercase());
    Ok(list)
}

/// The printer called `name`, or 404.
pub async fn find_printer(ipp: &ipp::Client, name: &str) -> Result<Printer> {
    if !valid_queue_name(name) {
        return Err(AppError::bad_request("invalid printer name"));
    }
    printers(ipp)
        .await?
        .into_iter()
        .find(|p| p.name == name)
        .ok_or_else(|| AppError::not_found(format!("there is no printer called {name}")))
}

pub async fn default_printer(ipp: &ipp::Client) -> Option<String> {
    let request = ipp::Request::new(ipp::CUPS_GET_DEFAULT)
        .keywords("requested-attributes", &["printer-name"]);
    let response = ipp.call("/", request).await.ok().filter(|r| r.ok())?;
    response
        .groups(ipp::GROUP_PRINTER)
        .find_map(|g| g.str("printer-name").map(str::to_string))
}

#[derive(Debug, Clone, Serialize)]
pub struct Job {
    pub id: i32,
    pub name: String,
    pub user: String,
    pub printer: String,
    /// pending, held, printing, stopped, canceled, aborted, completed
    pub state: &'static str,
    pub reasons: Vec<String>,
    pub size_kb: i32,
    /// Unix times
    pub created: i64,
    pub completed: Option<i64>,
    pub pages: i32,
    /// What the printer's backend last said about the job, e.g.
    /// "USB backend error: read failed (Operation timed out)".
    pub message: String,
    /// Held by CUPS rather than by someone: see `held_by_cups`.
    pub stuck: bool,
}

/// A held job that nobody asked to hold (no job-hold-until): CUPS holds a job
/// like this when printing it failed, e.g. because the printer did not
/// answer, and retries it every few minutes (printer-error-policy retry-job),
/// or when it waits for a password that will never come.
pub fn held_by_cups(state: &str, reasons: &[String]) -> bool {
    state == "held" && !reasons.iter().any(|r| r == "job-hold-until-specified")
}

const JOB_ATTRS: &[&str] = &[
    "job-id",
    "job-name",
    "job-originating-user-name",
    "job-printer-uri",
    "job-printer-state-message",
    "job-state",
    "job-state-reasons",
    "job-k-octets",
    "time-at-creation",
    "time-at-completed",
    "job-impressions-completed",
];

/// Active jobs, or finished ones (completed, canceled, aborted) when
/// `finished` is set, for all printers.
pub async fn jobs(ipp: &ipp::Client, finished: bool) -> Result<Vec<Job>> {
    let request = ipp::Request::new(ipp::GET_JOBS)
        .uri("printer-uri", "ipp://localhost/")
        .keyword(
            "which-jobs",
            if finished {
                "completed"
            } else {
                "not-completed"
            },
        )
        .boolean("my-jobs", false)
        .keywords("requested-attributes", JOB_ATTRS);
    let response = ipp.call("/", request).await.map_err(AppError::cups)?;
    if response.status == ipp::STATUS_NOT_FOUND {
        return Ok(Vec::new());
    }
    if !response.ok() {
        return Err(AppError::cups(response.message()));
    }
    Ok(response
        .groups(ipp::GROUP_JOB)
        .filter_map(|g| {
            let printer_uri = g.string("job-printer-uri");
            let state = match g.int("job-state") {
                Some(3) => "pending",
                Some(4) => "held",
                Some(5) => "printing",
                Some(6) => "stopped",
                Some(7) => "canceled",
                Some(8) => "aborted",
                _ => "completed",
            };
            let reasons: Vec<String> = g
                .strings("job-state-reasons")
                .into_iter()
                .filter(|r| r != "none")
                .collect();
            Some(Job {
                id: g.int("job-id")?,
                name: g.string("job-name"),
                user: g.string("job-originating-user-name"),
                printer: printer_uri
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_string(),
                state,
                stuck: held_by_cups(state, &reasons),
                reasons,
                size_kb: g.int("job-k-octets").unwrap_or(0),
                created: g.int("time-at-creation").unwrap_or(0) as i64,
                completed: g.int("time-at-completed").filter(|t| *t > 0).map(i64::from),
                pages: g.int("job-impressions-completed").unwrap_or(0),
                message: g.string("job-printer-state-message"),
            })
        })
        .collect())
}

/// Cancels active jobs: those on `printer` (all printers when None), and
/// with `stuck_only` just the ones CUPS holds after a failure that were
/// submitted at least `min_age` ago. Returns the ids of the canceled jobs.
pub async fn cancel_jobs(
    ipp: &ipp::Client,
    printer: Option<&str>,
    stuck_only: bool,
    min_age: Duration,
) -> Result<Vec<i32>> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut canceled = Vec::new();
    for job in jobs(ipp, false).await? {
        if printer.is_some_and(|p| p != job.printer)
            || (stuck_only && !job.stuck)
            || now - job.created < min_age.as_secs() as i64
        {
            continue;
        }
        match cancel(job.id as u32).await {
            Ok(()) => canceled.push(job.id),
            // It finished or was canceled in the meantime.
            Err(e) if e.message.contains("already") => {}
            Err(e) => return Err(e),
        }
    }
    Ok(canceled)
}

// ---------------------------------------------------------------------------
// Unwanted queues
// ---------------------------------------------------------------------------

/// Backends that reach printers over the network. One network printer often
/// has several queues on purpose, and their URIs say little about the device.
const NETWORK_SCHEMES: &[&str] = &[
    "ipp", "ipps", "http", "https", "socket", "lpd", "dnssd", "smb", "beh",
];

/// Scheme, make/model path and serial number of a local (USB-style) device
/// URI, lowercased: "captusb://Canon/LBP1210?drv=capt&serial=1" gives
/// ("captusb", "canon/lbp1210", Some("1")).
fn device_key(uri: &str) -> Option<(&str, String, Option<String>)> {
    let (scheme, rest) = uri.split_once("://")?;
    if NETWORK_SCHEMES.contains(&scheme) {
        return None;
    }
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let path = path.trim_end_matches('/').to_ascii_lowercase();
    if path.is_empty() {
        return None;
    }
    let serial = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("serial="))
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase);
    Some((scheme, path, serial))
}

/// Whether two device URIs reach the same local printer through different
/// backends, e.g. usb://Canon/LBP1210?serial=1 and
/// captusb://Canon/LBP1210?drv=capt&serial=1. install.sh has the same test
/// (`same_device_other_backend` in lib/cups.sh).
pub fn same_device_other_backend(a: &str, b: &str) -> bool {
    match (device_key(a), device_key(b)) {
        (Some((scheme_a, path_a, serial_a)), Some((scheme_b, path_b, serial_b))) => {
            scheme_a != scheme_b
                && path_a == path_b
                && (serial_a.is_none() || serial_b.is_none() || serial_a == serial_b)
        }
        _ => false,
    }
}

/// The queue a device URI forwards to when it points back at this server's
/// own scheduler: ipp://localhost/printers/NAME, ipps://HOST.local:631/...
fn loopback_target<'a>(uri: &'a str, hostname: &str) -> Option<&'a str> {
    let (scheme, rest) = uri.split_once("://")?;
    if !matches!(scheme, "ipp" | "ipps" | "http" | "https") {
        return None;
    }
    let (authority, path) = rest.split_once('/')?;
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = match host.strip_prefix('[') {
        Some(v6) => v6.split_once(']').map_or(v6, |(h, _)| h),
        None => host.split(':').next().unwrap_or(host),
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let hostname = hostname.to_ascii_lowercase();
    let local = ["localhost", "127.0.0.1", "::1"].contains(&host.as_str())
        || host == hostname
        || host == format!("{hostname}.local");
    if !local {
        return None;
    }
    let name = path.strip_prefix("printers/")?.split(['/', '?']).next()?;
    (!name.is_empty()).then_some(name)
}

/// Sets `unwanted` on queues that only get in the way:
///
/// - a queue that reaches a printer through a different backend than one of
///   reprinter's own queues, typically a `usb://` queue with a generic driver
///   that the desktop created when the printer was first plugged in. It
///   sends the printer data it does not understand, which can jam it until it
///   is switched off and on.
/// - a queue that forwards to another queue on this server (cups-browsed
///   creates these for the server's own shared printers).
///
/// `driver_schemes` are the backends of reprinter's drivers (DRIVER_URI_SCHEME,
/// e.g. "captusb"): queues on them are reprinter's own and never flagged.
pub fn flag_unwanted(printers: &mut [Printer], driver_schemes: &[String], hostname: &str) {
    let scheme = |p: &Printer| {
        p.device_uri
            .split(':')
            .next()
            .unwrap_or_default()
            .to_string()
    };
    let own: Vec<(String, String)> = printers
        .iter()
        .filter(|p| driver_schemes.contains(&scheme(p)))
        .map(|p| (p.name.clone(), p.device_uri.clone()))
        .collect();
    let names: HashSet<String> = printers.iter().map(|p| p.name.clone()).collect();
    for p in printers.iter_mut() {
        if own.iter().any(|(name, _)| *name == p.name) {
            continue;
        }
        if let Some((name, _)) = own
            .iter()
            .find(|(_, uri)| same_device_other_backend(uri, &p.device_uri))
        {
            let driver = if p.make_model.is_empty() {
                String::new()
            } else {
                format!(" with the driver \"{}\"", p.make_model)
            };
            p.unwanted = Some(format!(
                "It drives the same printer as {name}, but through the {} backend{driver}. \
                 Jobs sent to it can jam the printer.",
                scheme(p)
            ));
        } else if let Some(target) =
            loopback_target(&p.device_uri, hostname).filter(|t| *t != p.name && names.contains(*t))
        {
            p.unwanted = Some(format!(
                "A duplicate of {target}: it only passes its jobs on to {target} on this server."
            ));
        }
    }
}

/// Waits until the scheduler answers again (cupsctl restarts it).
pub async fn wait_ready(ipp: &ipp::Client) {
    for _ in 0..40 {
        if printers(ipp).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

// ---------------------------------------------------------------------------
// PPD options
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PpdOption {
    pub key: String,
    pub label: String,
    pub choices: Vec<String>,
    pub default: String,
}

/// Parses `lpoptions -l`: one option per line, `Key/Label: choice *default ...`.
/// Choice keywords contain no ':' or '/', so the last ':' ends the label.
pub fn parse_ppd_options(text: &str) -> Vec<PpdOption> {
    text.lines()
        .filter_map(|line| {
            let (head, values) = line.rsplit_once(':')?;
            let (key, label) = head.split_once('/').unwrap_or((head, head));
            let key = key.trim();
            if key.is_empty() || key.contains(char::is_whitespace) {
                return None;
            }
            let mut default = String::new();
            let mut choices = Vec::new();
            for token in values.split_whitespace() {
                let (is_default, choice) = match token.strip_prefix('*') {
                    Some(c) => (true, c),
                    None => (false, token),
                };
                if choice.starts_with("Custom.") {
                    continue; // custom sizes need a size form; not offered
                }
                if is_default {
                    default = choice.to_string();
                }
                choices.push(choice.to_string());
            }
            if choices.is_empty() {
                return None;
            }
            if default.is_empty() {
                default = choices[0].clone();
            }
            Some(PpdOption {
                key: key.to_string(),
                label: label.trim().to_string(),
                choices,
                default,
            })
        })
        .collect()
}

/// The printer's PPD options with their current defaults. Queues without a
/// PPD (raw queues) have none.
pub async fn ppd_options(printer: &str) -> Result<Vec<PpdOption>> {
    match run("lpoptions", &args(["-p", printer, "-l"]), QUICK).await {
        Ok(out) => Ok(parse_ppd_options(&out)),
        Err(e) if e.message.contains("Unable to get PPD") => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// Every key must be one of the printer's PPD options and every value one of
/// its choices. This is what keeps arbitrary `-o` options out of lp/lpadmin.
pub fn check_options(options: &BTreeMap<String, String>, ppd: &[PpdOption]) -> Result<()> {
    for (key, value) in options {
        let option = ppd
            .iter()
            .find(|o| &o.key == key)
            .ok_or_else(|| AppError::bad_request(format!("the printer has no option {key}")))?;
        if !option.choices.contains(value) {
            return Err(AppError::bad_request(format!(
                "{value} is not a valid choice for {}",
                option.label
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Jobs, server settings, devices, drivers (command-line tools)
// ---------------------------------------------------------------------------

/// Submits with `lp` and returns the job id, e.g. "LBP1210-12". With
/// `document`, the file is fed to lp on standard input: lp only honours
/// `-o document-format=...` for standard input, and guesses the type of files
/// named on its command line.
pub async fn submit(lp_args: &[String], document: Option<&Path>) -> Result<String> {
    let out = run_with_input("lp", lp_args, SLOW, document).await?;
    parse_request_id(&out)
        .ok_or_else(|| AppError::cups(format!("unexpected answer from lp: {}", out.trim())))
}

pub fn parse_request_id(out: &str) -> Option<String> {
    out.split("request id is ")
        .nth(1)?
        .split_whitespace()
        .next()
        .map(str::to_string)
}

pub async fn cancel(job_id: u32) -> Result<()> {
    run("cancel", &[job_id.to_string()], QUICK)
        .await
        .map(|_| ())
}

/// Releases a held job so CUPS tries it again now, instead of at its next
/// retry (or never, for a job held until released).
pub async fn release(job_id: u32) -> Result<()> {
    run(
        "lp",
        &args(["-i", &job_id.to_string(), "-H", "resume"]),
        QUICK,
    )
    .await
    .map(|_| ())
}

/// `cupsctl` with no arguments: `_share_printers=1` style lines.
pub async fn server_settings() -> Result<HashMap<String, String>> {
    Ok(parse_key_values(&run("cupsctl", &[], QUICK).await?))
}

pub fn parse_key_values(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct Device {
    pub uri: String,
    pub info: String,
    pub make_model: String,
    /// Set by callers that know which model they are looking for.
    pub matches: bool,
}

/// Devices the backend for `scheme` reports (`lpinfo -l -v`).
pub async fn devices(scheme: &str) -> Result<Vec<Device>> {
    if !valid_scheme(scheme) {
        return Err(AppError::bad_request(format!(
            "invalid device scheme {scheme}"
        )));
    }
    let out = run(
        "lpinfo",
        &args(["-l", "--include-schemes", scheme, "-v"]),
        SLOW,
    )
    .await?;
    Ok(parse_devices(&out))
}

pub fn parse_devices(text: &str) -> Vec<Device> {
    let mut list: Vec<Device> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(uri) = line.strip_prefix("Device: uri = ") {
            list.push(Device {
                uri: uri.trim().to_string(),
                info: String::new(),
                make_model: String::new(),
                matches: false,
            });
        } else if let Some(device) = list.last_mut() {
            if let Some(v) = line.strip_prefix("info = ") {
                device.info = v.trim().to_string();
            } else if let Some(v) = line.strip_prefix("make-and-model = ") {
                device.make_model = v.trim().to_string();
            }
        }
    }
    list
}

/// PPD names the installed drivers provide (`lpinfo -m`).
pub async fn models() -> Result<HashSet<String>> {
    let out = run("lpinfo", &args(["-m"]), SLOW).await?;
    Ok(out
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_string)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ppd_option_lines() {
        let text = "PageSize/Media Size: Letter Legal *A4 A5 Custom.WIDTHxHEIGHT\n\
                    Resolution/Output Resolution: *600dpi\n\
                    TonerDensity/Toner Density: 1 2 *3 4 5\n\
                    Weird/Label: with colon: a *b\n\
                    NoDefault: x y\n\
                    garbage line\n";
        let opts = parse_ppd_options(text);
        assert_eq!(opts.len(), 5);
        assert_eq!(
            opts[0],
            PpdOption {
                key: "PageSize".into(),
                label: "Media Size".into(),
                choices: vec!["Letter".into(), "Legal".into(), "A4".into(), "A5".into()],
                default: "A4".into(),
            }
        );
        assert_eq!(opts[2].default, "3");
        assert_eq!(opts[3].label, "Label: with colon");
        assert_eq!(opts[3].default, "b");
        assert_eq!(
            (opts[4].label.as_str(), opts[4].default.as_str()),
            ("NoDefault", "x")
        );
    }

    #[test]
    fn options_are_checked_against_the_ppd() {
        let ppd = parse_ppd_options("PageSize/Media Size: Letter *A4\n");
        let mut o = BTreeMap::new();
        o.insert("PageSize".to_string(), "Letter".to_string());
        assert!(check_options(&o, &ppd).is_ok());
        o.insert(
            "PageSize".to_string(),
            "A4 job-hold-until=indefinite".to_string(),
        );
        assert!(check_options(&o, &ppd).is_err());
        let mut o = BTreeMap::new();
        o.insert("document-format".to_string(), "application/pdf".to_string());
        assert!(check_options(&o, &ppd).is_err());
    }

    #[test]
    fn page_ranges() {
        assert_eq!(normalize_page_ranges("").unwrap(), None);
        assert_eq!(
            normalize_page_ranges(" 1-3, 5 ").unwrap().as_deref(),
            Some("1-3,5")
        );
        assert_eq!(
            normalize_page_ranges("7,1-3,2-4,5").unwrap().as_deref(),
            Some("1-5,7")
        );
        assert_eq!(normalize_page_ranges("4-4").unwrap().as_deref(), Some("4"));
        for bad in ["0", "3-1", "1-", "-2", "a", "1;2", "1-2-3", "1,,2"] {
            assert!(normalize_page_ranges(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn names_and_uris() {
        assert!(valid_queue_name("LBP1210"));
        assert!(valid_queue_name("Laser_2.a-b"));
        assert!(!valid_queue_name(""));
        assert!(!valid_queue_name("a b"));
        assert!(!valid_queue_name("../x"));
        assert!(valid_device_uri(
            "captusb://Canon/LBP1210?drv=capt&serial=123"
        ));
        assert!(valid_device_uri("usb://HP/LaserJet%201020"));
        assert!(!valid_device_uri("captusb://x y"));
        assert!(!valid_device_uri("-v"));
        assert!(!valid_device_uri("nocolon"));
        assert_eq!(clean_text(" a\nb\u{7}c ", 10), "abc");
    }

    #[test]
    fn same_printer_through_another_backend() {
        let capt = "captusb://Canon/LASER%20SHOT%20LBP-1210?drv=capt&serial=00000000";
        assert!(same_device_other_backend(
            capt,
            "usb://Canon/LASER%20SHOT%20LBP-1210?serial=00000000"
        ));
        // Case and a missing serial do not matter; another serial does.
        assert!(same_device_other_backend(
            capt,
            "usb://canon/laser%20shot%20lbp-1210"
        ));
        assert!(!same_device_other_backend(
            capt,
            "usb://Canon/LASER%20SHOT%20LBP-1210?serial=1234"
        ));
        // Same backend (a second queue on purpose), another model, network URIs.
        assert!(!same_device_other_backend(capt, capt));
        assert!(!same_device_other_backend(
            capt,
            "usb://Canon/LBP3200?serial=00000000"
        ));
        assert!(!same_device_other_backend(
            "ipp://10.0.0.5/ipp/print",
            "ipps://10.0.0.5/ipp/print"
        ));
        assert!(!same_device_other_backend(capt, "capture:/out"));
    }

    #[test]
    fn queues_that_point_back_at_this_server() {
        let t = |uri| loopback_target(uri, "printpi");
        assert_eq!(t("ipps://localhost:631/printers/LBP1210"), Some("LBP1210"));
        assert_eq!(
            t("ipp://127.0.0.1/printers/LBP1210?waitjob=false"),
            Some("LBP1210")
        );
        assert_eq!(t("ipp://[::1]:631/printers/A/"), Some("A"));
        assert_eq!(t("ipp://PrintPi.local./printers/A"), Some("A"));
        assert_eq!(t("ipp://printpi:631/printers/A"), Some("A"));
        assert_eq!(t("ipp://otherhost.local/printers/A"), None);
        assert_eq!(t("ipp://localhost/classes/A"), None);
        assert_eq!(t("socket://localhost:9100"), None);
    }

    fn queue(name: &str, uri: &str, make_model: &str) -> Printer {
        Printer {
            name: name.into(),
            info: String::new(),
            location: String::new(),
            make_model: make_model.into(),
            state: "idle",
            state_message: String::new(),
            state_reasons: Vec::new(),
            accepting: true,
            shared: true,
            is_default: false,
            device_uri: uri.into(),
            queued_jobs: 0,
            unwanted: None,
        }
    }

    #[test]
    fn unwanted_queues_are_flagged() {
        let mut list = vec![
            queue(
                "LBP1210",
                "captusb://Canon/LASER%20SHOT%20LBP-1210?drv=capt&serial=0",
                "Canon LBP1210, captppd 1.0.0",
            ),
            queue(
                "LASER-SHOT-LBP-1210",
                "usb://Canon/LASER%20SHOT%20LBP-1210?serial=0",
                "HP LaserJet Series PCL 4/5",
            ),
            queue("Copy", "ipps://localhost:631/printers/LBP1210", ""),
            queue("Self", "ipp://localhost/printers/Self", ""),
            queue(
                "Office",
                "usb://HP/LaserJet%201020?serial=9",
                "HP LaserJet 1020",
            ),
            queue("Network", "ipp://10.0.0.5/ipp/print", ""),
        ];
        flag_unwanted(&mut list, &["captusb".to_string()], "printpi");
        let flags: Vec<(&str, bool)> = list
            .iter()
            .map(|p| (p.name.as_str(), p.unwanted.is_some()))
            .collect();
        assert_eq!(
            flags,
            [
                ("LBP1210", false),
                ("LASER-SHOT-LBP-1210", true),
                ("Copy", true),
                ("Self", false),
                ("Office", false),
                ("Network", false),
            ]
        );
        let reason = list[1].unwanted.as_deref().unwrap();
        assert!(
            reason.contains("LBP1210") && reason.contains("usb backend"),
            "{reason}"
        );
        assert!(reason.contains("HP LaserJet Series PCL 4/5"), "{reason}");

        // Without a driver backend there is nothing to compare with.
        let mut list = vec![
            queue("A", "usb://Canon/X?serial=1", ""),
            queue("B", "hp://Canon/X?serial=1", ""),
        ];
        flag_unwanted(&mut list, &[], "printpi");
        assert!(list.iter().all(|p| p.unwanted.is_none()));
    }

    #[test]
    fn jobs_held_by_cups() {
        let reasons = |r: &[&str]| r.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(held_by_cups("held", &reasons(&["resources-are-not-ready"])));
        assert!(held_by_cups(
            "held",
            &reasons(&["cups-held-for-authentication"])
        ));
        assert!(held_by_cups("held", &[]));
        // Held on purpose (lp -H hold, hold until a time), or not held at all.
        assert!(!held_by_cups(
            "held",
            &reasons(&["job-hold-until-specified"])
        ));
        assert!(!held_by_cups("pending", &reasons(&["printer-stopped"])));
        assert!(!held_by_cups("printing", &[]));
    }

    #[test]
    fn tool_output_parsers() {
        assert_eq!(
            parse_request_id("request id is LBP1210-5 (1 file(s))\n").as_deref(),
            Some("LBP1210-5")
        );
        assert_eq!(parse_request_id("nothing"), None);
        let kv = parse_key_values("_debug_logging=0\n_share_printers=1\nWebInterface=Yes\n");
        assert_eq!(kv["_share_printers"], "1");
        let devs = parse_devices(
            "Device: uri = captusb://Canon/LBP1210?drv=capt&serial=1\n\
             \tclass = direct\n\tinfo = Canon LBP1210\n\tmake-and-model = Canon LBP1210\n\
             \tdevice-id = MFG:Canon;\n\tlocation = \n\
             Device: uri = captusb://Canon/LBP3200?drv=capt\n\tinfo = Canon LBP3200\n",
        );
        assert_eq!(devs.len(), 2);
        assert_eq!(devs[0].make_model, "Canon LBP1210");
        assert_eq!(devs[1].info, "Canon LBP3200");
    }
}
