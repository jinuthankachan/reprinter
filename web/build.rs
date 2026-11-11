//! Embeds the installer and the printer support files from the repository
//! root (install.sh, lib/, drivers/, printers/) into the binary, so a single
//! reprinter-web file can set up a print server (`reprinter-web install`)
//! and knows every printer profile without a checkout next to it.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

const EMBED: &[&str] = &["install.sh", "lib", "drivers", "printers"];

fn collect(repo: &Path, path: &Path, out: &mut Vec<(String, PathBuf)>) {
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = fs::read_dir(path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for entry in entries {
            collect(repo, &entry, out);
        }
    } else if path.is_file() {
        let rel = path
            .strip_prefix(repo)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        out.push((rel, path.canonicalize().unwrap()));
    }
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let repo = manifest
        .parent()
        .expect("web/ lives in the reprinter repository");
    let mut files = Vec::new();
    for name in EMBED {
        let path = repo.join(name);
        assert!(
            path.exists(),
            "{} is missing; build from a reprinter checkout",
            path.display()
        );
        println!("cargo:rerun-if-changed={}", path.display());
        collect(repo, &path, &mut files);
    }
    let mut code = String::from(
        "/// (path relative to the repository root, contents)\npub static FILES: &[(&str, &str)] = &[\n",
    );
    for (rel, abs) in &files {
        code.push_str(&format!(
            "    ({rel:?}, include_str!({:?})),\n",
            abs.display().to_string()
        ));
    }
    code.push_str("];\n");
    fs::write(
        PathBuf::from(env::var("OUT_DIR").unwrap()).join("embedded.rs"),
        code,
    )
    .unwrap();
}
