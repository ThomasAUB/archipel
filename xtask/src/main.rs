//! Repository quality gates, run in CI (ARCHITECTURE §6):
//!
//! * `cargo xtask check-deps`: every crate only depends on the workspace
//!   crates it is allowed to (ARCHITECTURE §2.2), every allowed edge is
//!   used (the table stays an accurate map), and the third-party crates
//!   wrapped by an adapter are only used by that adapter (§2.3).
//!   Dev-dependencies are free (adapter tests may use fixtures and real
//!   backends).
//! * `cargo xtask check-colors`: no literal color in `archipel-ui` outside its
//!   theme module (PLAN §3.5: UI code reads semantic tokens only).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Allowed workspace dependencies per crate (normal and build dependencies).
const ALLOWED: &[(&str, &[&str])] = &[
    ("archipel-model", &[]),
    ("archipel-theme", &[]),
    ("archipel-fixtures", &[]),
    ("archipel-git", &["archipel-model"]),
    ("archipel-watch", &["archipel-model"]),
    ("archipel-cache", &["archipel-model"]),
    ("archipel-project", &["archipel-model"]),
    ("archipel-ops", &["archipel-model"]),
    ("archipel-forge", &["archipel-model"]),
    ("archipel-stats", &["archipel-model"]),
    // The graph uses the pure LRU of archipel-cache (no SQLite types).
    ("archipel-graph", &["archipel-model", "archipel-cache"]),
    (
        "archipel-app",
        &[
            "archipel-model",
            "archipel-ops",
            "archipel-graph",
            "archipel-stats",
            "archipel-project",
            "archipel-theme",
            "archipel-cache",
        ],
    ),
    // Composition roots know every implementation.
    ("archipel-compose", &["*"]),
    ("archipel-cli", &["*"]),
    // The UI never names an adapter; its `main` gets them through compose.
    (
        "archipel-ui",
        &[
            "archipel-app",
            "archipel-model",
            "archipel-theme",
            "archipel-graph",
            "archipel-ops",
            "archipel-compose",
        ],
    ),
    ("xtask", &[]),
];

/// Third-party crates wrapped by an adapter: only these workspace crates may
/// depend on them (ARCHITECTURE §2.3), the composition roots excepted.
const WRAPPED: &[(&str, &[&str])] = &[
    ("gix", &["archipel-git"]),
    ("rusqlite", &["archipel-cache"]),
    ("notify", &["archipel-watch"]),
    ("ureq", &["archipel-forge"]),
    ("keyring", &["archipel-forge"]),
    ("gpui-kit", &["archipel-ui"]),
];

fn main() -> Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("check-deps") => check_deps(),
        Some("check-colors") => check_colors(&workspace_root().join("crates/archipel-ui/src")),
        _ => bail!("usage: cargo xtask (check-deps | check-colors)"),
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn check_deps() -> Result<()> {
    let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(workspace_root())
        .output()
        .context("running cargo metadata")?;
    if !out.status.success() {
        bail!(
            "cargo metadata failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let meta: serde_json::Value =
        serde_json::from_slice(&out.stdout).context("parsing cargo metadata")?;
    let members: Vec<&serde_json::Value> = meta["packages"]
        .as_array()
        .context("no packages")?
        .iter()
        .collect();
    let names: Vec<&str> = members.iter().filter_map(|p| p["name"].as_str()).collect();
    let rules: BTreeMap<&str, &[&str]> = ALLOWED.iter().copied().collect();
    let mut errors = Vec::new();
    for package in &members {
        let name = package["name"].as_str().unwrap_or_default();
        let Some(allowed) = rules.get(name) else {
            errors.push(format!(
                "{name}: no dependency rule (add it to xtask ALLOWED and ARCHITECTURE.md)"
            ));
            continue;
        };
        let deps: Vec<&str> = package["dependencies"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|dep| dep["kind"].as_str() != Some("dev"))
            .filter_map(|dep| dep["name"].as_str())
            .collect();
        let wildcard = allowed.contains(&"*");
        for dep in deps.iter().filter(|d| names.contains(d)) {
            if !wildcard && !allowed.contains(dep) {
                errors.push(format!("{name} must not depend on {dep}"));
            }
        }
        for edge in allowed.iter().filter(|a| **a != "*" && !deps.contains(a)) {
            errors.push(format!(
                "{name}: allowed edge to {edge} is unused (remove it from ALLOWED and ARCHITECTURE.md)"
            ));
        }
        for (wrapped, owners) in WRAPPED {
            if !wildcard && deps.contains(wrapped) && !owners.contains(&name) {
                errors.push(format!(
                    "{name} must not depend on {wrapped}: only {} wraps it",
                    owners.join(", ")
                ));
            }
        }
    }
    if errors.is_empty() {
        println!("check-deps: {} crates OK", members.len());
        Ok(())
    } else {
        bail!("dependency rules violated:\n  {}", errors.join("\n  "))
    }
}

fn check_colors(src: &Path) -> Result<()> {
    if !src.exists() {
        println!(
            "check-colors: {} does not exist, nothing to check",
            src.display()
        );
        return Ok(());
    }
    let pattern = regex::Regex::new(
        r##"(rgba?\s*\(\s*0x|\bhsla?\s*\(|\brgb\s*\(|"#[0-9a-fA-F]{3,8}"|0x[0-9a-fA-F]{6}(?:[0-9a-fA-F]{2})?\b)"##,
    )?;
    let mut errors = Vec::new();
    let mut stack = vec![src.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path
                .strip_prefix(src)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            // Only the theme module itself maps tokens to colors.
            let theme_module = rel == "theme.rs" || rel.starts_with("theme/");
            if path.extension().is_none_or(|e| e != "rs") || theme_module {
                continue;
            }
            for (i, line) in std::fs::read_to_string(&path)?.lines().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                if pattern.is_match(line) {
                    errors.push(format!("{rel}:{}: literal color: {}", i + 1, line.trim()));
                }
            }
        }
    }
    if errors.is_empty() {
        println!("check-colors: OK");
        Ok(())
    } else {
        bail!(
            "use theme tokens instead of literal colors:\n  {}",
            errors.join("\n  ")
        )
    }
}
