//! Installs a release of Archipel in place of the running binary (the update
//! badge): picks the archive for this platform, checks it against the
//! release's `SHA256SUMS.txt`, unpacks it with the system `tar` and swaps the
//! binary.
//!
//! * The running process is never touched. Windows lets a running binary be
//!   renamed but not deleted, so it moves aside as `<name>.old`, removed at
//!   the next start; elsewhere the new binary is renamed over it (the running
//!   process keeps the old inode).
//! * Any failure before the swap leaves the installed binary as it was; a
//!   failed swap on Windows moves the old binary back.
//! * `tar` ships with Windows 10+ (`System32\tar.exe`, bsdtar: it also reads
//!   `.zip`), macOS and Linux, so no archive crate is needed. On Windows the
//!   one in `System32` is used: Git's GNU tar, often first on `PATH`, reads
//!   neither `.zip` nor `C:` paths.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use archipel_model::ForgeError;
use archipel_model::forge::{Release, ReleaseAsset};
use sha2::{Digest, Sha256};

use crate::http::HttpTransport;

/// Host of the downloads, for network errors.
const HOST: &str = "github.com";

/// The checksums asset of a release (`sha256sum` output).
const CHECKSUMS: &str = "SHA256SUMS.txt";

/// The binary's name inside an archive.
const BINARY: &str = if cfg!(windows) {
    "archipel.exe"
} else {
    "archipel"
};

/// The target triple of the release archive matching this build; `None`
/// where no archive is published (the release page is the only way there).
pub const TARGET: Option<&str> = if cfg!(all(windows, target_arch = "x86_64")) {
    Some("x86_64-pc-windows-msvc")
} else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    Some("x86_64-unknown-linux-gnu")
} else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    Some("aarch64-apple-darwin")
} else {
    None
};

fn update_error(message: impl Into<String>) -> ForgeError {
    ForgeError::Update(message.into())
}

fn network_error(message: String) -> ForgeError {
    ForgeError::Network {
        host: HOST.into(),
        message,
    }
}

/// The archive for `target` among `assets`:
/// `archipel-v<version>-<target>.zip` or `.tar.gz`.
#[must_use]
pub fn archive_for<'a>(assets: &'a [ReleaseAsset], target: &str) -> Option<&'a ReleaseAsset> {
    let suffix = format!("-{target}");
    assets.iter().find(|asset| {
        asset
            .name
            .strip_suffix(".zip")
            .or_else(|| asset.name.strip_suffix(".tar.gz"))
            .is_some_and(|stem| stem.starts_with("archipel-") && stem.ends_with(&suffix))
    })
}

/// The SHA-256 of `name` in a `sha256sum` listing (`<hex>  <name>`, or
/// `<hex> *<name>` for binary mode), lowercase.
#[must_use]
pub fn checksum_of(listing: &str, name: &str) -> Option<String> {
    listing.lines().find_map(|line| {
        let (hash, file) = line.trim().split_once(char::is_whitespace)?;
        let file = file.trim_start().trim_start_matches('*');
        (file == name && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

/// Lowercase hex of `bytes`.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// A file that hashes what is written to it.
struct HashingFile {
    file: File,
    hasher: Sha256,
}

impl Write for HashingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.file.write(buf)?;
        self.hasher.update(&buf[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// A scratch directory, removed when dropped.
struct WorkDir(PathBuf);

impl WorkDir {
    fn new(version: &str) -> Result<Self, ForgeError> {
        let path =
            std::env::temp_dir().join(format!("archipel-update-{version}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)
            .map_err(|e| update_error(format!("cannot create {}: {e}", path.display())))?;
        Ok(Self(path))
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The path the running binary is moved to on Windows (`archipel.exe.old`).
fn old_path(current: &Path) -> PathBuf {
    sibling(current, ".old")
}

/// The path the new binary is first copied to, next to the current one so
/// the swap is a rename on one volume (`archipel.exe.new`).
fn staged_path(current: &Path) -> PathBuf {
    sibling(current, ".new")
}

fn sibling(current: &Path, suffix: &str) -> PathBuf {
    let mut name = current.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    current.with_file_name(name)
}

/// Installs `release` in place of the running binary (see the module doc);
/// returns the binary's path.
pub(crate) fn install(
    http: &dyn HttpTransport,
    release: &Release,
    progress: &dyn Fn(u8),
) -> Result<PathBuf, ForgeError> {
    let current = std::env::current_exe()
        .map_err(|e| update_error(format!("cannot locate the running binary: {e}")))?;
    install_into(http, release, &current, progress)?;
    Ok(current)
}

/// Installs `release` in place of the binary at `current`.
fn install_into(
    http: &dyn HttpTransport,
    release: &Release,
    current: &Path,
    progress: &dyn Fn(u8),
) -> Result<(), ForgeError> {
    let target = TARGET.ok_or_else(|| update_error("no release archive for this platform"))?;
    let archive = archive_for(&release.assets, target).ok_or_else(|| {
        update_error(format!(
            "release {} has no archive for {target}",
            release.version
        ))
    })?;
    let sums = release
        .assets
        .iter()
        .find(|asset| asset.name == CHECKSUMS)
        .ok_or_else(|| update_error(format!("release {} has no {CHECKSUMS}", release.version)))?;
    let mut listing = Vec::new();
    http.download(&sums.url, &mut listing, &mut |_, _| {})
        .map_err(network_error)?;
    let expected = checksum_of(&String::from_utf8_lossy(&listing), &archive.name)
        .ok_or_else(|| update_error(format!("{CHECKSUMS} does not list {}", archive.name)))?;

    let work = WorkDir::new(&release.version)?;
    let archive_path = work.0.join(&archive.name);
    let file = File::create(&archive_path)
        .map_err(|e| update_error(format!("cannot create {}: {e}", archive_path.display())))?;
    let mut out = HashingFile {
        file,
        hasher: Sha256::new(),
    };
    let mut shown = None;
    http.download(&archive.url, &mut out, &mut |received, total| {
        let total = total.unwrap_or(archive.size).max(1);
        let share = u8::try_from((received.saturating_mul(100) / total).min(100)).unwrap_or(100);
        if shown != Some(share) {
            shown = Some(share);
            progress(share);
        }
    })
    .map_err(network_error)?;
    out.flush()
        .map_err(|e| update_error(format!("cannot write {}: {e}", archive_path.display())))?;
    let HashingFile { file, hasher } = out;
    drop(file);
    if hex(&hasher.finalize()) != expected {
        return Err(update_error(format!(
            "{} does not match its checksum",
            archive.name
        )));
    }

    let unpacked = work.0.join("unpacked");
    std::fs::create_dir_all(&unpacked)
        .map_err(|e| update_error(format!("cannot create {}: {e}", unpacked.display())))?;
    unpack(&archive_path, &unpacked)?;
    let binary = find_binary(&unpacked)
        .ok_or_else(|| update_error(format!("{} holds no {BINARY}", archive.name)))?;
    replace(current, &binary)
}

/// Removes what an earlier install left: the binary moved aside on Windows,
/// a staged copy of an interrupted install.
pub(crate) fn clean_up() {
    if let Ok(current) = std::env::current_exe() {
        let _ = std::fs::remove_file(old_path(&current));
        let _ = std::fs::remove_file(staged_path(&current));
    }
}

/// The `tar` to run (see the module doc).
fn tar_program() -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("SystemRoot")
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
            .join("System32")
            .join("tar.exe")
    } else {
        PathBuf::from("tar")
    }
}

/// Unpacks `archive` (`.zip` or `.tar.gz`) into `into`.
fn unpack(archive: &Path, into: &Path) -> Result<(), ForgeError> {
    let mut cmd = Command::new(tar_program());
    cmd.arg("-xf").arg(archive).arg("-C").arg(into);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // No console window flashing from the GUI app.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd
        .output()
        .map_err(|e| update_error(format!("cannot run tar: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(update_error(format!(
            "cannot unpack {}: {}",
            archive.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// The binary in an unpacked archive: at its root or in its top folder
/// (`archipel-v0.2.0-<target>/archipel`).
fn find_binary(unpacked: &Path) -> Option<PathBuf> {
    let at_root = unpacked.join(BINARY);
    if at_root.is_file() {
        return Some(at_root);
    }
    std::fs::read_dir(unpacked)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(BINARY))
        .find(|path| path.is_file())
}

/// Puts `new` in place of `current` (see the module doc).
fn replace(current: &Path, new: &Path) -> Result<(), ForgeError> {
    let staged = staged_path(current);
    std::fs::copy(new, &staged)
        .map_err(|e| update_error(format!("cannot write {}: {e}", staged.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755));
    }
    let swapped = if cfg!(windows) {
        let old = old_path(current);
        let _ = std::fs::remove_file(&old);
        std::fs::rename(current, &old).and_then(|()| {
            std::fs::rename(&staged, current).inspect_err(|_| {
                let _ = std::fs::rename(&old, current);
            })
        })
    } else {
        std::fs::rename(&staged, current)
    };
    swapped.map_err(|e| {
        let _ = std::fs::remove_file(&staged);
        update_error(format!("cannot replace {}: {e}", current.display()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> ReleaseAsset {
        ReleaseAsset {
            name: name.into(),
            url: format!("https://example.com/{name}"),
            size: 1,
        }
    }

    #[test]
    fn the_archive_of_each_platform() {
        let assets = [
            asset("SHA256SUMS.txt"),
            asset("archipel-v0.2.0-x86_64-pc-windows-msvc.zip"),
            asset("archipel-v0.2.0-x86_64-unknown-linux-gnu.tar.gz"),
            asset("archipel-v0.2.0-aarch64-apple-darwin.tar.gz"),
        ];
        let name = |target| archive_for(&assets, target).map(|a| a.name.as_str());
        assert_eq!(
            name("x86_64-pc-windows-msvc"),
            Some("archipel-v0.2.0-x86_64-pc-windows-msvc.zip")
        );
        assert_eq!(
            name("aarch64-apple-darwin"),
            Some("archipel-v0.2.0-aarch64-apple-darwin.tar.gz")
        );
        assert_eq!(name("x86_64-apple-darwin"), None);
        assert_eq!(name("riscv64gc-unknown-linux-gnu"), None);
    }

    #[test]
    fn checksums_by_file_name() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let listing = format!("{a}  archipel.zip\n{b} *archipel.tar.gz\nbad  other.zip\n");
        assert_eq!(checksum_of(&listing, "archipel.zip"), Some(a));
        assert_eq!(
            checksum_of(&listing, "archipel.tar.gz"),
            Some("b".repeat(64))
        );
        assert_eq!(checksum_of(&listing, "other.zip"), None);
        assert_eq!(checksum_of(&listing, "missing.zip"), None);
    }

    #[test]
    fn hex_of_a_digest() {
        assert_eq!(
            hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn replace_swaps_the_binary_and_clean_up_paths_are_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join(BINARY);
        let new = dir.path().join("downloaded");
        std::fs::write(&current, "old").unwrap();
        std::fs::write(&new, "new").unwrap();
        replace(&current, &new).unwrap();
        assert_eq!(std::fs::read_to_string(&current).unwrap(), "new");
        assert!(!staged_path(&current).exists());
        if cfg!(windows) {
            assert_eq!(std::fs::read_to_string(old_path(&current)).unwrap(), "old");
        }
        assert_eq!(old_path(&current), dir.path().join(format!("{BINARY}.old")));
    }

    /// The whole path against the published releases (real asset names,
    /// download redirects, checksums, the system `tar`), into a scratch
    /// file: `cargo test -p archipel-forge --lib -- --ignored latest`.
    #[test]
    #[ignore = "network: downloads the latest published release"]
    fn installs_the_latest_published_release() {
        use std::sync::{Arc, Mutex};

        use archipel_model::ports::ReleaseFeed as _;

        use crate::http::UreqTransport;
        use crate::releases::GitHubReleases;

        let http: Arc<dyn HttpTransport> =
            Arc::new(UreqTransport::new(std::time::Duration::from_secs(30)));
        let feed = GitHubReleases::from_repository_url(http.clone(), env!("CARGO_PKG_REPOSITORY"))
            .unwrap();
        let release = feed.latest_release().unwrap().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join(BINARY);
        std::fs::write(&current, "old").unwrap();
        let shown = Mutex::new(Vec::new());
        install_into(http.as_ref(), &release, &current, &|share| {
            shown.lock().unwrap().push(share);
        })
        .unwrap();
        assert!(std::fs::metadata(&current).unwrap().len() > 1_000_000);
        assert_eq!(shown.lock().unwrap().last(), Some(&100));
    }

    #[test]
    fn find_binary_at_the_root_or_in_the_top_folder() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(find_binary(dir.path()), None);
        let top = dir.path().join("archipel-v0.2.0-target");
        std::fs::create_dir(&top).unwrap();
        std::fs::write(top.join(BINARY), "bin").unwrap();
        assert_eq!(find_binary(dir.path()), Some(top.join(BINARY)));
    }
}
