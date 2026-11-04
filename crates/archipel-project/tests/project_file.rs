#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `archipel.toml` round trips, error cases and private-project delegation.

use std::fs;
use std::path::{Path, PathBuf};

use archipel_model::error::ProjectError;
use archipel_model::ids::RepoPath;
use archipel_model::ports::project::ProjectSource;
use archipel_model::project::{
    MessageTemplateDef, ProjectDef, ProjectLocation, ProjectSettings, RepoEntry,
};
use archipel_project::{FileProjectSource, PROJECT_FILE_NAME};

fn source() -> FileProjectSource {
    FileProjectSource::new()
}

fn entry(path: PathBuf, alias: Option<&str>) -> RepoEntry {
    RepoEntry {
        path: RepoPath::new(path),
        alias: alias.map(str::to_owned),
        category: None,
    }
}

fn sample(root: &Path) -> ProjectDef {
    ProjectDef {
        name: "My product".to_owned(),
        repos: vec![
            entry(root.join("proj").join("app"), None),
            RepoEntry {
                category: Some("libs".to_owned()),
                ..entry(root.join("shared").join("libfoo"), Some("libfoo"))
            },
            entry(root.join("proj"), Some("root")),
        ],
        settings: ProjectSettings {
            require_same_branch: true,
            ticket_prefix: Some("JIRA-1".to_owned()),
            ticket_from_branch: Some(r"([A-Z]+-\d+)".to_owned()),
            message_templates: vec![MessageTemplateDef {
                name: "fix".to_owned(),
                body: "{ticket} fix: \"quoted\"\nsecond line".to_owned(),
            }],
            accent: Some("#3b82f6".to_owned()),
            icon: Some("🚀".to_owned()),
        },
    }
}

fn shared(dir: &Path) -> (PathBuf, ProjectLocation) {
    let file = dir.join("proj").join(PROJECT_FILE_NAME);
    (file.clone(), ProjectLocation::SharedFile(file))
}

fn load_text(text: &str) -> (tempfile::TempDir, Result<ProjectDef, ProjectError>) {
    let dir = tempfile::tempdir().unwrap();
    let (file, location) = shared(dir.path());
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, text).unwrap();
    let result = source().load(&location);
    (dir, result)
}

#[test]
fn round_trip_load_save_load() {
    let dir = tempfile::tempdir().unwrap();
    let (file, location) = shared(dir.path());
    let project = sample(dir.path());
    let src = source();

    src.save(&location, &project).unwrap();
    let loaded = src.load(&location).unwrap();
    assert_eq!(loaded, project);

    let first = fs::read_to_string(&file).unwrap();
    src.save(&location, &loaded).unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), first, "save is stable");
    assert_eq!(src.load(&location).unwrap(), project);
}

#[test]
fn saved_paths_are_relative_with_forward_slashes() {
    let dir = tempfile::tempdir().unwrap();
    let (file, location) = shared(dir.path());
    source().save(&location, &sample(dir.path())).unwrap();
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.contains("version = 1"), "{text}");
    assert!(text.contains(r#"path = "app""#), "{text}");
    assert!(text.contains(r#"path = "../shared/libfoo""#), "{text}");
    assert!(text.contains(r#"path = ".""#), "{text}");
    assert!(
        text.lines()
            .filter(|l| l.starts_with("path"))
            .all(|l| !l.contains('\\')),
        "{text}"
    );
    assert!(text.contains("[[settings.template]]"), "{text}");
    assert!(text.contains(r#"category = "libs""#), "{text}");
    assert_eq!(text.matches("category").count(), 1, "{text}");
}

#[test]
fn blank_category_is_no_category() {
    let (_dir, result) = load_text(
        "version = 1\nname = \"x\"\n[[repo]]\npath = \"a\"\ncategory = \"  \"\n\
         [[repo]]\npath = \"b\"\ncategory = \" libs \"\n",
    );
    let repos = result.unwrap().repos;
    assert_eq!(repos[0].category, None);
    assert_eq!(repos[1].category.as_deref(), Some("libs"));
}

#[test]
fn minimal_project_omits_settings() {
    let dir = tempfile::tempdir().unwrap();
    let (file, location) = shared(dir.path());
    let project = ProjectDef {
        name: "Empty".to_owned(),
        repos: Vec::new(),
        settings: ProjectSettings::default(),
    };
    source().save(&location, &project).unwrap();
    let text = fs::read_to_string(&file).unwrap();
    assert!(!text.contains("settings"), "{text}");
    assert_eq!(source().load(&location).unwrap(), project);
}

#[test]
fn loads_plan_example_with_dot_dot_and_windows_separators() {
    let (dir, result) = load_text(
        r#"
version = 1
name = "My product"

[[repo]]
path = "app"

[[repo]]
path = '..\shared\libfoo'
alias = "libfoo"

[[repo]]
path = "./tools/../cli/"
"#,
    );
    let project = result.unwrap();
    let root = dir.path();
    assert_eq!(project.name, "My product");
    assert_eq!(
        project.repos,
        vec![
            entry(root.join("proj").join("app"), None),
            entry(root.join("shared").join("libfoo"), Some("libfoo")),
            entry(root.join("proj").join("cli"), None),
        ]
    );
    assert_eq!(project.settings, ProjectSettings::default());
}

#[test]
fn missing_repositories_are_not_errors() {
    let (_dir, result) =
        load_text("version = 1\nname = \"x\"\n[[repo]]\npath = \"does/not/exist\"\n");
    assert_eq!(result.unwrap().repos.len(), 1);
}

#[test]
fn duplicate_repositories_are_deduplicated() {
    let (_dir, result) = load_text(
        r#"
version = 1
name = "dup"
[[repo]]
path = "app"
alias = "first"
[[repo]]
path = "./app/"
[[repo]]
path = "other/../app"
"#,
    );
    let repos = result.unwrap().repos;
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0].alias.as_deref(), Some("first"));
}

#[cfg(windows)]
#[test]
fn other_drive_is_stored_absolute() {
    let dir = tempfile::tempdir().unwrap();
    let (file, location) = shared(dir.path());
    let drive = if dir.path().to_string_lossy().starts_with(['Z', 'z']) {
        r"Y:\elsewhere\repo"
    } else {
        r"Z:\elsewhere\repo"
    };
    let project = ProjectDef {
        name: "drives".to_owned(),
        repos: vec![entry(PathBuf::from(drive), None)],
        settings: ProjectSettings::default(),
    };
    source().save(&location, &project).unwrap();
    let text = fs::read_to_string(&file).unwrap();
    assert!(
        text.contains(":\\\\elsewhere") || text.contains(r":\elsewhere"),
        "{text}"
    );
    assert_eq!(source().load(&location).unwrap(), project);
}

#[test]
fn absolute_path_in_file_is_kept() {
    let other = tempfile::tempdir().unwrap();
    let abs = other.path().join("repo");
    let text = format!(
        "version = 1\nname = \"abs\"\n[[repo]]\npath = '{}'\n",
        abs.display()
    );
    let (_dir, result) = load_text(&text);
    assert_eq!(result.unwrap().repos, vec![entry(abs, None)]);
}

#[test]
fn future_version_is_unsupported() {
    let (_dir, result) = load_text("version = 2\nname = \"x\"\n[[repo]]\nlocation = \"y\"\n");
    assert_eq!(
        result.unwrap_err(),
        ProjectError::UnsupportedVersion {
            found: 2,
            supported: 1
        }
    );
}

fn invalid_message(result: Result<ProjectDef, ProjectError>) -> String {
    match result {
        Err(ProjectError::Invalid { message, .. }) => message,
        other => panic!("expected Invalid, got {other:?}"),
    }
}

#[test]
fn malformed_toml_is_invalid() {
    let (_dir, result) = load_text("version = 1\nname = \"unterminated\n");
    assert!(!invalid_message(result).is_empty());
}

#[test]
fn missing_name_is_invalid() {
    let (_dir, result) = load_text("version = 1\n");
    assert!(invalid_message(result).contains("name"));
}

#[test]
fn empty_name_is_invalid() {
    let (_dir, result) = load_text("version = 1\nname = \"  \"\n");
    assert!(invalid_message(result).contains("name"));
}

#[test]
fn missing_or_bad_version_is_invalid() {
    let (_dir, result) = load_text("name = \"x\"\n");
    assert!(invalid_message(result).contains("version"));
    let (_dir, result) = load_text("version = \"one\"\nname = \"x\"\n");
    assert!(invalid_message(result).contains("version"));
    let (_dir, result) = load_text("version = 0\nname = \"x\"\n");
    assert!(invalid_message(result).contains("version"));
}

#[test]
fn wrong_field_type_is_invalid() {
    let (_dir, result) = load_text("version = 1\nname = \"x\"\n[[repo]]\nalias = \"no path\"\n");
    assert!(invalid_message(result).contains("path"));
}

#[test]
fn missing_file_is_io_error() {
    let dir = tempfile::tempdir().unwrap();
    let (_, location) = shared(dir.path());
    assert!(matches!(
        source().load(&location),
        Err(ProjectError::Io { .. })
    ));
}

#[test]
fn save_overwrites_atomically_without_leftovers() {
    let dir = tempfile::tempdir().unwrap();
    let (file, location) = shared(dir.path());
    let mut project = sample(dir.path());
    source().save(&location, &project).unwrap();
    project.name = "Renamed".to_owned();
    source().save(&location, &project).unwrap();
    assert_eq!(source().load(&location).unwrap().name, "Renamed");
    let names: Vec<_> = fs::read_dir(file.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, [PROJECT_FILE_NAME]);
}
