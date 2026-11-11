//! The repository files compiled into the binary by build.rs: install.sh,
//! lib/, drivers/ and printers/.

use std::{io, path::Path};

include!(concat!(env!("OUT_DIR"), "/embedded.rs"));

pub fn get(path: &str) -> Option<&'static str> {
    FILES
        .iter()
        .find(|(p, _)| *p == path)
        .map(|(_, text)| *text)
}

/// Files directly inside `dir` (e.g. "printers"): (file name, contents).
pub fn list(dir: &str) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
    FILES.iter().filter_map(move |(path, text)| {
        let name = path.strip_prefix(dir)?.strip_prefix('/')?;
        (!name.contains('/')).then_some((name, *text))
    })
}

/// Writes all embedded files under `root`, recreating the repository layout.
pub fn extract(root: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    for (path, text) in FILES {
        let target = root.join(path);
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&target, text)?;
        let mode = if path.ends_with(".sh") { 0o755 } else { 0o644 };
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_files_are_embedded() {
        assert!(
            get("install.sh")
                .unwrap()
                .starts_with("#!/usr/bin/env bash")
        );
        assert!(get("lib/common.sh").is_some());
        assert!(get("lib/cups.sh").is_some());
        assert!(list("printers").any(|(name, _)| name == "canon-lbp1210.conf"));
        assert!(list("drivers").any(|(name, _)| name == "captppd.sh"));
        assert!(list("lib").all(|(name, _)| !name.contains('/')));
    }

    #[test]
    fn extracts_a_runnable_tree() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("reprinter-extract-{}", std::process::id()));
        extract(&dir).unwrap();
        let mode = std::fs::metadata(dir.join("install.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
        assert!(dir.join("printers/canon-lbp1210.conf").is_file());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
