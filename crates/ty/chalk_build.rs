use std::{fs, path::Path};

/// Retrieve the Ruff version from its package manifest.
pub(super) fn ruff_version_info(workspace_root: &Path) {
    let manifest = workspace_root.join("crates/ruff/Cargo.toml");
    println!("cargo:rerun-if-changed={}", manifest.display());

    let Ok(manifest) = fs::read_to_string(manifest) else {
        return;
    };
    let mut in_package = false;
    for line in manifest.lines() {
        match line {
            "[package]" => in_package = true,
            line if line.starts_with('[') => in_package = false,
            line if in_package && line.starts_with("version =") => {
                let Some((_key, version)) = line.split_once('=') else {
                    continue;
                };
                println!(
                    "cargo::rustc-env=TY_RUFF_VERSION={}",
                    version.trim().trim_matches('"')
                );
                return;
            }
            _ => {}
        }
    }
}
