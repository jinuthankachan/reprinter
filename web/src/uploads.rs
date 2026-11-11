//! Files uploaded for printing.
//!
//! Each upload is stored under a random id in the uploads directory, so it
//! can be previewed and printed more than once (manual two-sided printing
//! prints the same file twice). Metadata is kept in memory: files left over
//! from a previous run are deleted at startup, and unused uploads expire.
//!
//! Uploading is open to everyone on the network, so the space uploads take
//! is bounded and they cannot fill the disk: at most `MAX_FILES` files, and
//! at most `STORAGE_FACTOR` times the upload limit in total. Files still
//! being received count too.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, SystemTime},
};

use serde::Serialize;

pub const MAX_FILES: usize = 100;
pub const STORAGE_FACTOR: u64 = 10;

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
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    items: HashMap<String, Upload>,
    /// Size of the stored files plus what has arrived of those being received.
    bytes: u64,
    /// Files being received.
    receiving: usize,
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
            state: Mutex::default(),
        })
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    pub fn path(&self, id: &str) -> PathBuf {
        self.dir.join(id)
    }

    pub fn get(&self, id: &str) -> Option<Upload> {
        self.state.lock().unwrap().items.get(id).cloned()
    }

    /// Starts receiving a file under a new id, or None when `MAX_FILES`
    /// files are already stored or being received.
    pub fn receive(&self) -> Option<Incoming<'_>> {
        let mut s = self.state.lock().unwrap();
        if s.items.len() + s.receiving >= MAX_FILES {
            return None;
        }
        s.receiving += 1;
        let id = crate::auth::random_hex(16);
        Some(Incoming {
            store: self,
            path: self.path(&id),
            id,
            bytes: 0,
            done: false,
        })
    }

    /// Restarts the expiry clock (the file was just printed).
    pub fn touch(&self, id: &str) {
        if let Some(u) = self.state.lock().unwrap().items.get_mut(id) {
            u.used = SystemTime::now();
        }
    }

    pub fn remove(&self, id: &str) -> bool {
        let removed = {
            let mut s = self.state.lock().unwrap();
            let removed = s.items.remove(id);
            if let Some(u) = &removed {
                s.bytes -= u.size;
            }
            removed
        };
        if let Some(u) = &removed {
            let _ = std::fs::remove_file(self.path(&u.id));
        }
        removed.is_some()
    }

    pub fn prune(&self, ttl: Duration) -> usize {
        let now = SystemTime::now();
        let expired: Vec<String> = self
            .state
            .lock()
            .unwrap()
            .items
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

/// A file being received (`Uploads::receive`). Its bytes count against the
/// storage limit as they arrive; the file is deleted again, and its room
/// given back, unless `finish` is called.
pub struct Incoming<'a> {
    store: &'a Uploads,
    pub id: String,
    pub path: PathBuf,
    bytes: u64,
    done: bool,
}

impl Incoming<'_> {
    /// Takes room for `n` more bytes; false when all uploads together would
    /// then take more than `limit` bytes.
    pub fn reserve(&mut self, n: u64, limit: u64) -> bool {
        let mut s = self.store.state.lock().unwrap();
        if s.bytes.saturating_add(n) > limit {
            return false;
        }
        s.bytes += n;
        self.bytes += n;
        true
    }

    /// Keeps the file as an upload of the size received so far.
    pub fn finish(mut self, name: String, kind: Kind, mime: &'static str) -> Upload {
        let upload = Upload {
            id: self.id.clone(),
            name,
            size: self.bytes,
            kind,
            mime,
            used: SystemTime::now(),
        };
        let store = self.store;
        let mut s = store.state.lock().unwrap();
        s.receiving -= 1;
        s.items.insert(upload.id.clone(), upload.clone());
        self.done = true;
        upload
    }
}

impl Drop for Incoming<'_> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let _ = std::fs::remove_file(&self.path);
        let mut s = self.store.state.lock().unwrap();
        s.receiving -= 1;
        s.bytes -= self.bytes;
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

        let mut incoming = store.receive().unwrap();
        std::fs::write(&incoming.path, b"x").unwrap();
        assert!(incoming.reserve(1, 100));
        let id = incoming.finish("x".into(), Kind::Text, "text/plain").id;
        assert!(is_upload_id(&id));
        assert_eq!(store.get(&id).unwrap().size, 1);
        store.state.lock().unwrap().items.get_mut(&id).unwrap().used =
            SystemTime::now() - Duration::from_secs(120);
        assert_eq!(store.prune(Duration::from_secs(3600)), 0);
        assert_eq!(store.prune(Duration::from_secs(60)), 1);
        assert!(store.get(&id).is_none());
        assert!(!store.path(&id).exists());
        assert_eq!(store.state.lock().unwrap().bytes, 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn storage_is_bounded() {
        let dir = std::env::temp_dir().join(format!("reprinter-quota-{}", std::process::id()));
        let store = Uploads::new(dir.clone()).unwrap();

        // Bytes: in-flight uploads count, abandoned ones give their room back.
        let mut a = store.receive().unwrap();
        assert!(a.reserve(60, 100));
        let mut b = store.receive().unwrap();
        assert!(!b.reserve(50, 100), "60 + 50 is over the limit");
        assert!(b.reserve(40, 100));
        drop(b);
        assert_eq!(store.state.lock().unwrap().bytes, 60);
        let a = a.finish("a".into(), Kind::Text, "text/plain");
        let mut c = store.receive().unwrap();
        assert!(!c.reserve(41, 100));
        drop(c);
        store.remove(&a.id);
        assert_eq!(store.state.lock().unwrap().bytes, 0);

        // Files: stored and in-flight ones count.
        let pending: Vec<_> = (0..MAX_FILES).map(|_| store.receive().unwrap()).collect();
        assert!(store.receive().is_none());
        drop(pending);
        assert!(store.receive().is_some());
        assert_eq!(store.state.lock().unwrap().receiving, 0);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
