//! Settings of the web interface itself, kept in a small TOML file that the
//! Settings page rewrites. Printer and CUPS settings live in CUPS, not here.

use std::{io, ops::RangeInclusive, path::Path};

use serde::{Deserialize, Serialize};

pub const MAX_UPLOAD_MB: RangeInclusive<u64> = 1..=1024;
pub const UPLOAD_TTL_MINUTES: RangeInclusive<u64> = 5..=10080;
/// 0 turns the automatic cancellation of stuck jobs off.
pub const STUCK_JOB_MINUTES: RangeInclusive<u64> = 0..=10080;

const HEADER: &str = "# reprinter-web settings. The Settings page rewrites this file;\n\
                      # restart reprinter-web after editing it by hand.\n\n";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Address and port to listen on. Changing it needs a restart.
    pub listen: String,
    /// Largest file that can be uploaded for printing.
    pub max_upload_mb: u64,
    /// Uploaded files are deleted after this long without being printed.
    pub upload_ttl_minutes: u64,
    /// Jobs CUPS holds after a failure (the printer did not answer, ...) are
    /// canceled when they are older than this, so they do not sit in the
    /// queue and print hours later. 0: never.
    pub stuck_job_minutes: u64,
    /// Argon2 hash of the admin password. Empty: the settings are open.
    pub admin_password_hash: String,
    /// Extra host names the page may be opened by (e.g. "printer.lan"),
    /// besides IP addresses, localhost, the host name and HOSTNAME.local.
    /// Requests for any other name are refused (see `auth::host_allowed`).
    pub allowed_hosts: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:8631".into(),
            max_upload_mb: 50,
            upload_ttl_minutes: 60,
            stuck_job_minutes: 30,
            admin_password_hash: String::new(),
            allowed_hosts: Vec::new(),
        }
    }
}

impl Config {
    /// Reads the file, or returns the defaults when it does not exist yet.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let mut config: Config =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        config.clamp();
        Ok(config)
    }

    pub fn clamp(&mut self) {
        self.max_upload_mb = self
            .max_upload_mb
            .clamp(*MAX_UPLOAD_MB.start(), *MAX_UPLOAD_MB.end());
        self.upload_ttl_minutes = self
            .upload_ttl_minutes
            .clamp(*UPLOAD_TTL_MINUTES.start(), *UPLOAD_TTL_MINUTES.end());
        self.stuck_job_minutes = self
            .stuck_job_minutes
            .clamp(*STUCK_JOB_MINUTES.start(), *STUCK_JOB_MINUTES.end());
    }

    /// Writes atomically (temporary file, then rename), readable only by the
    /// owner because it holds the password hash.
    ///
    /// The temporary file is always a new one. `reprinter-web install` saves
    /// as root in a directory the service user owns, so whatever is left at
    /// the temporary name (say, a link to /etc/passwd) is removed rather than
    /// written through.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let body = toml::to_string_pretty(self).map_err(io::Error::other)?;
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        match std::fs::remove_file(&tmp) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        // create_new (O_EXCL) fails on anything that appeared in the meantime,
        // links included, instead of following it.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(HEADER.as_bytes())?;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_keys_take_defaults_and_values_are_clamped() {
        let mut c: Config =
            toml::from_str("max_upload_mb = 0\nupload_ttl_minutes = 999999").unwrap();
        c.clamp();
        assert_eq!(c.listen, "0.0.0.0:8631");
        assert_eq!(c.max_upload_mb, 1);
        assert_eq!(c.upload_ttl_minutes, 10080);
        assert_eq!(c.stuck_job_minutes, 30);
        let mut c: Config = toml::from_str("stuck_job_minutes = 0").unwrap();
        c.clamp();
        assert_eq!(c.stuck_job_minutes, 0);
        assert!(c.admin_password_hash.is_empty());
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = std::env::temp_dir().join(format!("reprinter-config-{}", std::process::id()));
        let path = dir.join("config.toml");
        let c = Config {
            max_upload_mb: 20,
            admin_password_hash: "x".into(),
            ..Config::default()
        };
        c.save(&path).unwrap();
        let back = Config::load(&path).unwrap();
        assert_eq!(back.max_upload_mb, 20);
        assert_eq!(back.admin_password_hash, "x");
        assert!(back.allowed_hosts.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn save_does_not_write_through_a_planted_link() {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("reprinter-config-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let victim = dir.join("victim");
        std::fs::write(&victim, "untouched").unwrap();
        let path = dir.join("config.toml");
        std::os::unix::fs::symlink(&victim, path.with_extension("toml.tmp")).unwrap();

        Config::default().save(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched");
        assert!(!std::fs::symlink_metadata(&path).unwrap().is_symlink());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(Config::load(&path).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
