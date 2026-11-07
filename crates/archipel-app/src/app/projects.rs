//! Known projects: the registry (create, import, remove) and the ways to
//! start a project from a folder (spec §1 "Launch from a directory").

use std::path::{Path, PathBuf};

use archipel_model::project::{
    DiscoveredRepo, ProjectDef, ProjectLocation, ProjectSettings, RegistryEntry, RepoEntry,
    ScanOptions,
};
use archipel_model::{CancelToken, ProjectError, ProjectId, RepoPath};

use super::{App, AppError, AppEvent, lock};

/// What opening a directory leads to (spec §1 "Launch from a directory").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryTarget {
    /// The project that opens.
    pub id: ProjectId,
    /// The `archipel.toml` found in the directory, if any.
    pub project_file: Option<PathBuf>,
    /// The folder scanned for repositories, where the project file is
    /// written when there is none: the directory, or the outermost
    /// repository containing it.
    pub scan_root: PathBuf,
}

/// Id of the project registered for a shared project file.
#[must_use]
pub fn file_project_id(file: &Path) -> ProjectId {
    ProjectId(format!("file:{}", file.display()))
}

/// `dir` made absolute and normalized.
fn normalize_dir(dir: &Path) -> Result<PathBuf, AppError> {
    Ok(
        archipel_project::normalize_dir(dir).map_err(|e| ProjectError::Io {
            path: dir.to_path_buf(),
            message: e.to_string(),
        })?,
    )
}

/// A new project definition named `name`, with the automatic accent of
/// project `id` written down so it stays when the file moves.
fn new_definition(name: String, id: &ProjectId, repos: Vec<RepoEntry>) -> ProjectDef {
    ProjectDef {
        name,
        repos,
        settings: ProjectSettings {
            accent: Some(archipel_theme::accent_for_key(&id.0).to_hex()),
            ..ProjectSettings::default()
        },
    }
}

impl App {
    /// Known projects, most recently opened first.
    pub fn projects(&self) -> Result<Vec<RegistryEntry>, AppError> {
        let mut list = self.services.registry.list_projects()?;
        list.sort_by(|a, b| {
            b.last_opened
                .cmp(&a.last_opened)
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(list)
    }

    /// Creates a project and registers it. Returns its id.
    pub fn create_project(
        &self,
        location: ProjectLocation,
        def: &ProjectDef,
    ) -> Result<ProjectId, AppError> {
        self.services.projects.save(&location, def)?;
        self.register(location, def.name.clone())
    }

    /// Registers an existing `archipel.toml` (e.g. from a teammate).
    pub fn import_project_file(&self, path: &Path) -> Result<ProjectId, AppError> {
        let location = ProjectLocation::SharedFile(path.to_path_buf());
        let def = self.services.projects.load(&location)?;
        self.register(location, def.name)
    }

    /// Adds (or updates) the registry entry of the project file at
    /// `location`, never opened yet. Returns its id.
    fn register(&self, location: ProjectLocation, name: String) -> Result<ProjectId, AppError> {
        let ProjectLocation::SharedFile(path) = &location;
        let id = file_project_id(path);
        self.services.registry.upsert_project(&RegistryEntry {
            id: id.clone(),
            name,
            location,
            last_opened: None,
        })?;
        self.emit(AppEvent::ProjectsChanged);
        Ok(id)
    }

    /// Creates a shared project for a folder: writes `<dir>/archipel.toml`
    /// listing the repos found below it, registers and opens it. A folder
    /// that already holds a project file opens that file instead (never
    /// overwritten).
    pub fn new_project_from_folder(&self, dir: &Path) -> Result<ProjectId, AppError> {
        let dir = normalize_dir(dir)?;
        let file = dir.join(archipel_project::PROJECT_FILE_NAME);
        let id = if file.is_file() {
            self.import_project_file(&file)?
        } else {
            let repos = self.scan(&dir)?;
            if repos.is_empty() {
                return Err(AppError::NotFound(format!(
                    "no repository found below {}",
                    dir.display()
                )));
            }
            let def = new_definition(
                dir.file_name()
                    .map_or_else(|| "Project".into(), |n| n.to_string_lossy().into_owned()),
                &file_project_id(&file),
                repos
                    .into_iter()
                    .map(|r| RepoEntry {
                        path: r.path,
                        alias: None,
                        category: None,
                    })
                    .collect(),
            );
            self.create_project(ProjectLocation::SharedFile(file), &def)?
        };
        self.open_project(&id)?;
        Ok(id)
    }

    /// Removes a project from the registry (never deletes files).
    pub fn remove_project(&self, id: &ProjectId) -> Result<(), AppError> {
        if lock(&self.session).as_ref().is_some_and(|s| &s.id == id) {
            self.close_project();
        }
        self.services.registry.remove_project(id)?;
        self.emit(AppEvent::ProjectsChanged);
        Ok(())
    }

    /// Scans a folder for repositories (blocking; run off the UI thread or
    /// through the scheduler for big trees — the scan is fast, see PLAN Phase 1).
    pub fn scan(&self, root: &Path) -> Result<Vec<DiscoveredRepo>, AppError> {
        Ok(self
            .services
            .discovery
            .scan(root, &ScanOptions::default(), &CancelToken::new())?)
    }

    /// Adds a folder to the open project: the folder itself when it is a
    /// repository, else every repository found below it. Blocking (scans);
    /// run it off the UI thread.
    pub fn add_folder(&self, dir: &Path) -> Result<(), AppError> {
        let paths: Vec<RepoPath> = if dir.join(".git").exists() {
            vec![RepoPath::new(dir)]
        } else {
            self.scan(dir)?.into_iter().map(|r| r.path).collect()
        };
        self.add_repos(&paths)
    }

    /// What opening `dir` would open, without opening it (lets a frontend
    /// focus the window already showing that project).
    pub fn directory_target(&self, dir: &Path) -> Result<DirectoryTarget, AppError> {
        let dir = normalize_dir(dir)?;
        if !dir.is_dir() {
            return Err(AppError::NotFound(format!(
                "{} is not a directory",
                dir.display()
            )));
        }
        let file = dir.join(archipel_project::PROJECT_FILE_NAME);
        if file.is_file() {
            return Ok(DirectoryTarget {
                id: file_project_id(&file),
                project_file: Some(file),
                scan_root: dir,
            });
        }
        let root = archipel_project::outermost_repo_root(&dir).unwrap_or(dir);
        let file = root.join(archipel_project::PROJECT_FILE_NAME);
        Ok(DirectoryTarget {
            id: file_project_id(&file),
            project_file: file.is_file().then_some(file),
            scan_root: root,
        })
    }

    /// Opens the app on a directory (spec §1 "Launch from a directory"):
    /// its `archipel.toml` (or the one at the outermost enclosing repo root)
    /// when there is one. Otherwise writes `archipel.toml` at the scan root,
    /// shows the project at once and fills it in by a background,
    /// cancellable scan; relaunching then opens that file.
    pub fn open_directory(&self, dir: &Path) -> Result<ProjectId, AppError> {
        let target = self.directory_target(dir)?;
        if let Some(file) = &target.project_file {
            let id = self.import_project_file(file)?;
            self.open_project(&id)?;
            return Ok(id);
        }
        let name = target.scan_root.file_name().map_or_else(
            || target.scan_root.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let def = new_definition(name, &target.id, Vec::new());
        let file = target.scan_root.join(archipel_project::PROJECT_FILE_NAME);
        let id = self.create_project(ProjectLocation::SharedFile(file), &def)?;
        self.open_project(&id)?;
        self.start_scan(&id, target.scan_root);
        Ok(id)
    }
}
