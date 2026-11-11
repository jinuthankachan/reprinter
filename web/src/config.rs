//! Settings of the web interface itself, kept in a small TOML file that the
//! Settings page rewrites. Printer and CUPS settings live in CUPS, not here.

use std::{io, ops::RangeInclusive, path::Path};

use serde::{Deserialize, Serialize};

pub const MAX_UPLOAD_MB: RangeInclusive<u64> = 1..=1024;
pub const UPLOAD_TTL_MINUTES: RangeInclusive<u64> = 5..=10080;

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
    /// Argon2 hash of the admin password. Empty: the settings are open.
    pub admin_password_hash: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:8631".into(),
            max_upload_mb: 50,
            upload_ttl_minutes: 60,
            admin_password_hash: String::new(),
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
    }

    /// Writes atomically (temporary file, then rename), readable only by the
    /// owner because it holds the password hash.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let body = toml::to_string_pretty(self).map_err(io::Error::other)?;
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
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
        std::fs::remove_dir_all(dir).unwrap();
    }
}
