//! Files uploaded for printing.
//!
//! Each upload is stored under a random id in the uploads directory, so it
//! can be previewed and printed more than once (manual two-sided printing
//! prints the same file twice). Metadata is kept in memory: files left over
//! from a previous run are deleted at startup, and unused uploads expire.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, SystemTime},
};

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Pdf,
    Postscript,
    Image,
    Text,
}

#[derive(Debug, Clone, Serialize)]
pub struct Upload {
    pub id: String,
    pub name: String,
    pub size: u64,
    pub kind: Kind,
    pub mime: &'static str,
    #[serde(skip)]
    pub used: SystemTime,
}

pub struct Uploads {
    dir: PathBuf,
    items: Mutex<HashMap<String, Upload>>,
}

fn is_upload_id(name: &str) -> bool {
    name.len() == 32 && name.chars().all(|c| c.is_ascii_hexdigit())
}

impl Uploads {
    pub fn new(dir: PathBuf) -> std::io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(&dir)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        // Absolute, independent of the working directory.
        let dir = std::fs::canonicalize(&dir)?;
        for entry in std::fs::read_dir(&dir)?.flatten() {
            if entry.file_name().to_str().is_some_and(is_upload_id) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        Ok(Self {
            dir,
            items: Mutex::default(),
        })
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    pub fn path(&self, id: &str) -> PathBuf {
        self.dir.join(id)
    }

    pub fn get(&self, id: &str) -> Option<Upload> {
        self.items.lock().unwrap().get(id).cloned()
    }

    pub fn insert(&self, upload: Upload) {
        self.items.lock().unwrap().insert(upload.id.clone(), upload);
    }

    /// Restarts the expiry clock (the file was just printed).
    pub fn touch(&self, id: &str) {
        if let Some(u) = self.items.lock().unwrap().get_mut(id) {
            u.used = SystemTime::now();
        }
    }

    pub fn remove(&self, id: &str) -> bool {
        let removed = self.items.lock().unwrap().remove(id);
        if let Some(u) = &removed {
            let _ = std::fs::remove_file(self.path(&u.id));
        }
        removed.is_some()
    }

    pub fn prune(&self, ttl: Duration) -> usize {
        let now = SystemTime::now();
        let expired: Vec<String> = self
            .items
            .lock()
            .unwrap()
            .values()
            .filter(|u| now.duration_since(u.used).unwrap_or_default() > ttl)
            .map(|u| u.id.clone())
            .collect();
        for id in &expired {
            self.remove(id);
        }
        expired.len()
    }
}

/// Deletes a half-written upload unless disarmed (`.0 = None`).
pub struct RemoveOnDrop(pub Option<PathBuf>);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Recognises the formats CUPS can print out of the box (cups-filters and
/// Ghostscript) from the first bytes of the file. Anything else (office
/// documents, HTML, ...) is refused instead of failing later in the queue.
pub fn sniff(head: &[u8]) -> Option<(Kind, &'static str)> {
    let starts = |magic: &[u8]| head.starts_with(magic);
    // PDF allows junk before the header within the first 1024 bytes.
    if head[..head.len().min(1024)]
        .windows(5)
        .any(|w| w == b"%PDF-")
    {
        return Some((Kind::Pdf, "application/pdf"));
    }
    if starts(b"%!") || starts(&[0xC5, 0xD0, 0xD3, 0xC6]) {
        return Some((Kind::Postscript, "application/postscript"));
    }
    if starts(&[0xFF, 0xD8, 0xFF]) {
        return Some((Kind::Image, "image/jpeg"));
    }
    if starts(b"\x89PNG\r\n\x1a\n") {
        return Some((Kind::Image, "image/png"));
    }
    if starts(b"GIF87a") || starts(b"GIF89a") {
        return Some((Kind::Image, "image/gif"));
    }
    if starts(b"II*\0") || starts(b"MM\0*") {
        return Some((Kind::Image, "image/tiff"));
    }
    if looks_like_text(head) {
        return Some((Kind::Text, "text/plain"));
    }
    None
}

/// UTF-8 without NULs or control characters other than tab, newlines and
/// form feed. The sample may end in the middle of a multi-byte character.
fn looks_like_text(head: &[u8]) -> bool {
    let text = match std::str::from_utf8(head) {
        Ok(t) => t,
        Err(e) if e.error_len().is_none() => {
            std::str::from_utf8(&head[..e.valid_up_to()]).unwrap_or("")
        }
        Err(_) => return false,
    };
    !text.is_empty()
        && text
            .chars()
            .all(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r' | '\x0c'))
}

/// A display name for the upload: the base name the browser sent, without
/// control characters, bounded.
pub fn clean_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = base.chars().filter(|c| !c.is_control()).take(150).collect();
    let clean = clean.trim();
    if clean.is_empty() {
        "document".to_string()
    } else {
        clean.to_string()
    }
}

/// RFC 5987 encoding for `Content-Disposition: inline; filename*=UTF-8''...`.
pub fn pct_encode(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_supported_formats() {
        assert_eq!(sniff(b"%PDF-1.7\n...").unwrap().1, "application/pdf");
        assert_eq!(sniff(b"\xEF\xBB\xBF junk %PDF-1.4").unwrap().0, Kind::Pdf);
        assert_eq!(sniff(b"%!PS-Adobe-3.0\n").unwrap().0, Kind::Postscript);
        assert_eq!(sniff(b"\xFF\xD8\xFF\xE0..JFIF").unwrap().1, "image/jpeg");
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n....").unwrap().1, "image/png");
        assert_eq!(sniff(b"GIF89a....").unwrap().1, "image/gif");
        assert_eq!(sniff(b"MM\0*....").unwrap().1, "image/tiff");
        assert_eq!(
            sniff("hello\tworld\nÜmlaut\x0c".as_bytes()).unwrap().0,
            Kind::Text
        );
        // Cut in the middle of "é" (0xC3 0xA9).
        assert_eq!(sniff(b"caf\xC3").unwrap().0, Kind::Text);
    }

    #[test]
    fn refuses_other_formats() {
        assert!(sniff(b"PK\x03\x04 docx").is_none());
        assert!(sniff(b"\xD0\xCF\x11\xE0 doc").is_none());
        assert!(sniff(b"text with \0 nul").is_none());
        assert!(sniff(b"bad \xFF\xFE utf8 here").is_none());
        assert!(sniff(b"\x1b[31m escape").is_none());
        assert!(sniff(b"").is_none());
    }

    #[test]
    fn names() {
        assert_eq!(clean_name("C:\\Users\\me\\report.pdf"), "report.pdf");
        assert_eq!(clean_name("../../etc/passwd"), "passwd");
        assert_eq!(clean_name("a\nb.txt"), "ab.txt");
        assert_eq!(clean_name("  "), "document");
        assert_eq!(pct_encode("Über cv.pdf"), "%C3%9Cber%20cv.pdf");
    }

    #[test]
    fn store_lifecycle() {
        let dir = std::env::temp_dir().join(format!("reprinter-uploads-{}", std::process::id()));
        let leftover = dir.join("0123456789abcdef0123456789abcdef");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&leftover, b"old").unwrap();
        std::fs::write(dir.join("keep.txt"), b"not ours").unwrap();
        let store = Uploads::new(dir.clone()).unwrap();
        assert!(!leftover.exists(), "files from a previous run are removed");
        assert!(dir.join("keep.txt").exists(), "other files are left alone");

        let id = "fedcba9876543210fedcba9876543210".to_string();
        std::fs::write(store.path(&id), b"x").unwrap();
        store.insert(Upload {
            id: id.clone(),
            name: "x".into(),
            size: 1,
            kind: Kind::Text,
            mime: "text/plain",
            used: SystemTime::now() - Duration::from_secs(120),
        });
        assert_eq!(store.prune(Duration::from_secs(3600)), 0);
        assert_eq!(store.prune(Duration::from_secs(60)), 1);
        assert!(store.get(&id).is_none());
        assert!(!store.path(&id).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
