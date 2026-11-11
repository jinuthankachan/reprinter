//! Printer profiles (`printers/<id>.conf`) and driver modules
//! (`drivers/<name>.sh`), the same files install.sh uses.
//!
//! They are bash files of plain assignments (docs/adding-a-printer.md), so a
//! small parser for `NAME=value` lines is enough; nothing is executed.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Profile {
    pub id: String,
    pub model: String,
    pub usb_ids: Vec<String>,
    pub driver: String,
    pub ppd: String,
    #[serde(skip)]
    pub uri_regex: String,
    pub queue: String,
    pub options: Vec<String>,
    /// DRIVER_URI_SCHEME of the driver module, e.g. "captusb".
    pub uri_scheme: String,
}

/// All valid profiles under `<share_dir>/printers`, sorted by id. Broken
/// profiles are logged and skipped.
pub fn load_all(share_dir: &Path) -> Vec<Profile> {
    let Ok(entries) = std::fs::read_dir(share_dir.join("printers")) else {
        return Vec::new();
    };
    let mut list: Vec<Profile> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()? != "conf" {
                return None;
            }
            let id = path.file_stem()?.to_str()?.to_string();
            let text = std::fs::read_to_string(&path).ok()?;
            match parse_profile(&id, &text, share_dir) {
                Ok(p) => Some(p),
                Err(e) => {
                    eprintln!("profile {}: {e}", path.display());
                    None
                }
            }
        })
        .collect();
    list.sort_by(|a, b| a.id.cmp(&b.id));
    list
}

pub fn find(share_dir: &Path, id: &str) -> Option<Profile> {
    load_all(share_dir).into_iter().find(|p| p.id == id)
}

fn parse_profile(id: &str, text: &str, share_dir: &Path) -> Result<Profile, String> {
    let vars = parse_assignments(text);
    let get = |k: &str| vars.get(k).cloned().unwrap_or_default();
    for key in [
        "PRINTER_MODEL",
        "PRINTER_DRIVER",
        "PRINTER_PPD",
        "PRINTER_URI_REGEX",
        "PRINTER_QUEUE",
    ] {
        if get(key).is_empty() {
            return Err(format!("{key} is not set"));
        }
    }
    let driver = get("PRINTER_DRIVER");
    if !driver
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("invalid driver name {driver}"));
    }
    let uri_scheme =
        std::fs::read_to_string(share_dir.join("drivers").join(format!("{driver}.sh")))
            .ok()
            .and_then(|t| parse_assignments(&t).remove("DRIVER_URI_SCHEME"))
            .unwrap_or_default();
    Ok(Profile {
        id: id.to_string(),
        model: get("PRINTER_MODEL"),
        usb_ids: get("PRINTER_USB_IDS")
            .split_whitespace()
            .map(str::to_lowercase)
            .collect(),
        driver,
        ppd: get("PRINTER_PPD"),
        uri_regex: get("PRINTER_URI_REGEX"),
        queue: get("PRINTER_QUEUE"),
        options: get("PRINTER_OPTIONS")
            .split_whitespace()
            .map(str::to_string)
            .collect(),
        uri_scheme,
    })
}

/// `NAME=value` lines with upper-case names; values may be bare, 'single' or
/// "double" quoted. Lines inside functions (`local x`, lower-case names) are
/// ignored.
pub fn parse_assignments(text: &str) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    for line in text.lines() {
        let Some((name, rest)) = line.trim_start().split_once('=') else {
            continue;
        };
        let valid_name = name.starts_with(|c: char| c.is_ascii_uppercase() || c == '_')
            && name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        if !valid_name {
            continue;
        }
        if let Some(value) = parse_shell_word(rest) {
            vars.insert(name.to_string(), value);
        }
    }
    vars
}

/// One shell word (quotes removed). None when a quote is not closed.
fn parse_shell_word(text: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => loop {
                match chars.next()? {
                    '\'' => break,
                    ch => out.push(ch),
                }
            },
            '"' => loop {
                match chars.next()? {
                    '"' => break,
                    '\\' => {
                        let next = chars.next()?;
                        if !matches!(next, '"' | '\\' | '$' | '`') {
                            out.push('\\');
                        }
                        out.push(next);
                    }
                    ch => out.push(ch),
                }
            },
            '\\' => out.push(chars.next()?),
            c if c.is_whitespace() => break, // a trailing comment may follow
            c => out.push(c),
        }
    }
    Some(out)
}

/// `vendor:product` IDs (lowercase hex) of every attached USB device, from sysfs.
pub fn usb_devices() -> HashSet<String> {
    let mut ids = HashSet::new();
    let Ok(entries) = std::fs::read_dir("/sys/bus/usb/devices") else {
        return ids;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let read = |f: &str| {
            std::fs::read_to_string(dir.join(f))
                .ok()
                .map(|s| s.trim().to_lowercase())
        };
        if let (Some(vendor), Some(product)) = (read("idVendor"), read("idProduct")) {
            ids.insert(format!("{vendor}:{product}"));
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_assignments() {
        let text = r#"
# comment
PRINTER_MODEL="Canon LASER SHOT LBP-1210"
PRINTER_URI_REGEX='^captusb://[^/]+/[^?]*LBP-?1210[?]'
PRINTER_QUEUE=LBP1210
CAPTPPD_COMMIT="d6df" # tag v1.0.0
ESCAPED="a \"b\" \$c \n"
UNTERMINATED="oops
driver_install() {
  local status
  status="$(dpkg-query)"
}
"#;
        let v = parse_assignments(text);
        assert_eq!(v["PRINTER_MODEL"], "Canon LASER SHOT LBP-1210");
        assert_eq!(v["PRINTER_URI_REGEX"], "^captusb://[^/]+/[^?]*LBP-?1210[?]");
        assert_eq!(v["PRINTER_QUEUE"], "LBP1210");
        assert_eq!(v["CAPTPPD_COMMIT"], "d6df");
        assert_eq!(v["ESCAPED"], r#"a "b" $c \n"#);
        assert!(!v.contains_key("UNTERMINATED"));
        assert!(!v.contains_key("status"));
    }

    #[test]
    fn repository_profiles_load() {
        // The crate lives in web/; the profiles in the repository root.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let profiles = load_all(root);
        let lbp = profiles
            .iter()
            .find(|p| p.id == "canon-lbp1210")
            .expect("canon-lbp1210 profile");
        assert_eq!(lbp.driver, "captppd");
        assert_eq!(lbp.uri_scheme, "captusb");
        assert_eq!(lbp.usb_ids, vec!["04a9:2617"]);
        assert_eq!(lbp.options, vec!["Resolution=600dpi"]);
        let re = regex::Regex::new(&lbp.uri_regex).unwrap();
        assert!(re.is_match("captusb://Canon/LBP1210?drv=capt&serial=0001"));
        assert!(!re.is_match("captusb://Canon/LBP3200?drv=capt"));
    }
}
