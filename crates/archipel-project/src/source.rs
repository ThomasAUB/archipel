//! [`FileProjectSource`]: `archipel.toml` project files.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use archipel_model::error::ProjectError;
use archipel_model::ports::project::ProjectSource;
use archipel_model::project::{ProjectDef, ProjectLocation};

use crate::format;

/// Suffix of the temporary file written next to the target before the rename.
const TEMP_SUFFIX: &str = ".tmp";

/// Loads and saves project definitions: reads and writes the TOML file;
/// writes are atomic (temporary file in the same folder, then rename).
#[derive(Debug, Default)]
pub struct FileProjectSource;

impl FileProjectSource {
    /// Creates the source.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl ProjectSource for FileProjectSource {
    fn load(&self, location: &ProjectLocation) -> Result<ProjectDef, ProjectError> {
        let ProjectLocation::SharedFile(path) = location;
        let file = absolute(path)?;
        let text = fs::read_to_string(&file).map_err(|e| io_error(&file, &e))?;
        format::parse(&file, &text)
    }

    fn save(&self, location: &ProjectLocation, project: &ProjectDef) -> Result<(), ProjectError> {
        let ProjectLocation::SharedFile(path) = location;
        let file = absolute(path)?;
        let text = format::render(&file, project)?;
        write_atomic(&file, text.as_bytes()).map_err(|e| io_error(&file, &e))
    }
}

/// Absolute, lexically normalized form of `path`.
fn absolute(path: &Path) -> Result<PathBuf, ProjectError> {
    std::path::absolute(path)
        .map(|p| crate::paths::normalize(&p))
        .map_err(|e| io_error(path, &e))
}

fn io_error(path: &Path, error: &io::Error) -> ProjectError {
    ProjectError::Io {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

/// Writes `bytes` to a temporary sibling of `file`, syncs it, then renames it
/// over `file`, so readers never observe a half-written file.
fn write_atomic(file: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = file
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut temp_name = file.file_name().unwrap_or_default().to_os_string();
    temp_name.push(TEMP_SUFFIX);
    let temp = parent.join(temp_name);
    let result = (|| {
        let mut out = fs::File::create(&temp)?;
        out.write_all(bytes)?;
        out.sync_all()?;
        drop(out);
        fs::rename(&temp, file)
    })();
    if result.is_err() {
        // Best-effort cleanup: the original error is what the caller needs.
        let _ = fs::remove_file(&temp);
    }
    result
}
