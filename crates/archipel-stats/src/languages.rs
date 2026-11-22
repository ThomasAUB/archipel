//! Programming language breakdown (issue #94): the share of each language in
//! a tree, by lines and by files, like the language bar of a GitHub repo
//! page.
//!
//! Languages are told from file extensions and a few well-known file names.
//! Data and prose formats (JSON, YAML, Markdown…) are not counted, nor are
//! binary files (left out by the Git read), vendored code and generated
//! files. The `linguist-generated`, `linguist-vendored`,
//! `linguist-documentation` and `linguist-language` attributes override
//! these rules, as on GitHub.
//!
//! A [`FolderFilter`] narrows the count to some folders (only `src`, without
//! `tests`…), ticked in the [`FolderNode`] tree of the statistics panel.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use archipel_model::stats::TreeFile;

/// A known language: its name and its usual color (RGB, as on GitHub).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Language {
    /// Display name.
    pub name: &'static str,
    /// Usual color (RGB).
    pub color: [u8; 3],
}

const fn lang(name: &'static str, color: [u8; 3]) -> Language {
    Language { name, color }
}

/// Color of a language set by `linguist-language` that is not in the table.
pub const UNKNOWN_COLOR: [u8; 3] = [0x8b, 0x8b, 0x8b];

const C: Language = lang("C", [0x55, 0x55, 0x55]);
const CPP: Language = lang("C++", [0xf3, 0x4b, 0x7d]);
const OBJC: Language = lang("Objective-C", [0x43, 0x8e, 0xff]);

/// Languages by extension (lowercase, without the dot). `h` is resolved per
/// tree (C, C++ or Objective-C), see [`breakdown`].
const BY_EXTENSION: &[(&str, Language)] = &[
    ("rs", lang("Rust", [0xde, 0xa5, 0x84])),
    ("c", C),
    ("cc", CPP),
    ("cpp", CPP),
    ("cxx", CPP),
    ("c++", CPP),
    ("hh", CPP),
    ("hpp", CPP),
    ("hxx", CPP),
    ("h++", CPP),
    ("inl", CPP),
    ("ipp", CPP),
    ("tpp", CPP),
    ("ino", CPP),
    ("m", OBJC),
    ("mm", lang("Objective-C++", [0x68, 0x66, 0xfb])),
    ("cs", lang("C#", [0x17, 0x86, 0x00])),
    ("fs", lang("F#", [0xb8, 0x45, 0xfc])),
    ("fsx", lang("F#", [0xb8, 0x45, 0xfc])),
    ("vb", lang("Visual Basic .NET", [0x94, 0x5d, 0xb7])),
    ("java", lang("Java", [0xb0, 0x72, 0x19])),
    ("kt", lang("Kotlin", [0xa9, 0x7b, 0xff])),
    ("kts", lang("Kotlin", [0xa9, 0x7b, 0xff])),
    ("scala", lang("Scala", [0xc2, 0x2d, 0x40])),
    ("sc", lang("Scala", [0xc2, 0x2d, 0x40])),
    ("groovy", lang("Groovy", [0x42, 0x98, 0xb8])),
    ("gradle", lang("Groovy", [0x42, 0x98, 0xb8])),
    ("clj", lang("Clojure", [0xdb, 0x58, 0x55])),
    ("cljs", lang("Clojure", [0xdb, 0x58, 0x55])),
    ("cljc", lang("Clojure", [0xdb, 0x58, 0x55])),
    ("go", lang("Go", [0x00, 0xad, 0xd8])),
    ("swift", lang("Swift", [0xf0, 0x51, 0x38])),
    ("dart", lang("Dart", [0x00, 0xb4, 0xab])),
    ("zig", lang("Zig", [0xec, 0x91, 0x5c])),
    ("nim", lang("Nim", [0xff, 0xc2, 0x00])),
    ("d", lang("D", [0xba, 0x59, 0x5e])),
    ("v", lang("Verilog", [0xb2, 0xb7, 0xf8])),
    ("sv", lang("SystemVerilog", [0xda, 0xe1, 0xc2])),
    ("vhd", lang("VHDL", [0xad, 0xb2, 0xcb])),
    ("vhdl", lang("VHDL", [0xad, 0xb2, 0xcb])),
    ("f", lang("Fortran", [0x4d, 0x41, 0xb1])),
    ("f90", lang("Fortran", [0x4d, 0x41, 0xb1])),
    ("f95", lang("Fortran", [0x4d, 0x41, 0xb1])),
    ("asm", lang("Assembly", [0x6e, 0x4c, 0x13])),
    ("s", lang("Assembly", [0x6e, 0x4c, 0x13])),
    ("cu", lang("Cuda", [0x3a, 0x4e, 0x3a])),
    ("cuh", lang("Cuda", [0x3a, 0x4e, 0x3a])),
    ("glsl", lang("GLSL", [0x55, 0x86, 0xa4])),
    ("vert", lang("GLSL", [0x55, 0x86, 0xa4])),
    ("frag", lang("GLSL", [0x55, 0x86, 0xa4])),
    ("hlsl", lang("HLSL", [0xaa, 0xce, 0x60])),
    ("wgsl", lang("WGSL", [0x1a, 0x5e, 0x9a])),
    ("metal", lang("Metal", [0x8f, 0x14, 0xe9])),
    ("py", lang("Python", [0x35, 0x72, 0xa5])),
    ("pyi", lang("Python", [0x35, 0x72, 0xa5])),
    ("pyx", lang("Cython", [0xfe, 0xdf, 0x5b])),
    ("rb", lang("Ruby", [0x70, 0x15, 0x16])),
    ("php", lang("PHP", [0x4f, 0x5d, 0x95])),
    ("pl", lang("Perl", [0x02, 0x98, 0xc3])),
    ("pm", lang("Perl", [0x02, 0x98, 0xc3])),
    ("lua", lang("Lua", [0x00, 0x00, 0x80])),
    ("r", lang("R", [0x19, 0x8c, 0xe7])),
    ("jl", lang("Julia", [0xa2, 0x70, 0xba])),
    ("ex", lang("Elixir", [0x6e, 0x4a, 0x7e])),
    ("exs", lang("Elixir", [0x6e, 0x4a, 0x7e])),
    ("erl", lang("Erlang", [0xb8, 0x39, 0x98])),
    ("hrl", lang("Erlang", [0xb8, 0x39, 0x98])),
    ("hs", lang("Haskell", [0x5e, 0x50, 0x86])),
    ("ml", lang("OCaml", [0xef, 0x7a, 0x08])),
    ("mli", lang("OCaml", [0xef, 0x7a, 0x08])),
    ("elm", lang("Elm", [0x60, 0xb5, 0xcc])),
    ("lisp", lang("Common Lisp", [0x3f, 0xb6, 0x8b])),
    ("el", lang("Emacs Lisp", [0xc0, 0x65, 0xdb])),
    ("scm", lang("Scheme", [0x1e, 0x4a, 0xec])),
    ("rkt", lang("Racket", [0x3c, 0x5c, 0xaa])),
    ("pas", lang("Pascal", [0xe3, 0xf1, 0x71])),
    ("ada", lang("Ada", [0x02, 0xf8, 0x8c])),
    ("adb", lang("Ada", [0x02, 0xf8, 0x8c])),
    ("ads", lang("Ada", [0x02, 0xf8, 0x8c])),
    ("js", lang("JavaScript", [0xf1, 0xe0, 0x5a])),
    ("mjs", lang("JavaScript", [0xf1, 0xe0, 0x5a])),
    ("cjs", lang("JavaScript", [0xf1, 0xe0, 0x5a])),
    ("jsx", lang("JavaScript", [0xf1, 0xe0, 0x5a])),
    ("ts", lang("TypeScript", [0x31, 0x78, 0xc6])),
    ("mts", lang("TypeScript", [0x31, 0x78, 0xc6])),
    ("cts", lang("TypeScript", [0x31, 0x78, 0xc6])),
    ("tsx", lang("TypeScript", [0x31, 0x78, 0xc6])),
    ("vue", lang("Vue", [0x41, 0xb8, 0x83])),
    ("svelte", lang("Svelte", [0xff, 0x3e, 0x00])),
    ("html", lang("HTML", [0xe3, 0x4c, 0x26])),
    ("htm", lang("HTML", [0xe3, 0x4c, 0x26])),
    ("css", lang("CSS", [0x66, 0x33, 0x99])),
    ("scss", lang("SCSS", [0xc6, 0x53, 0x8c])),
    ("sass", lang("Sass", [0xa5, 0x3b, 0x70])),
    ("less", lang("Less", [0x1d, 0x36, 0x5d])),
    ("sh", lang("Shell", [0x89, 0xe0, 0x51])),
    ("bash", lang("Shell", [0x89, 0xe0, 0x51])),
    ("zsh", lang("Shell", [0x89, 0xe0, 0x51])),
    ("fish", lang("Shell", [0x89, 0xe0, 0x51])),
    ("ps1", lang("PowerShell", [0x01, 0x24, 0x56])),
    ("psm1", lang("PowerShell", [0x01, 0x24, 0x56])),
    ("bat", lang("Batchfile", [0xc1, 0xf1, 0x2e])),
    ("cmd", lang("Batchfile", [0xc1, 0xf1, 0x2e])),
    ("cmake", lang("CMake", [0xda, 0x34, 0x34])),
    ("mk", lang("Makefile", [0x42, 0x78, 0x19])),
    ("mak", lang("Makefile", [0x42, 0x78, 0x19])),
    ("nix", lang("Nix", [0x7e, 0x7e, 0xff])),
    ("tf", lang("HCL", [0x84, 0x4f, 0xba])),
    ("hcl", lang("HCL", [0x84, 0x4f, 0xba])),
    ("sql", lang("SQL", [0xe3, 0x8c, 0x00])),
    ("proto", lang("Protocol Buffer", [0x00, 0x00, 0x00])),
    ("thrift", lang("Thrift", [0xd1, 0x20, 0x27])),
    ("graphql", lang("GraphQL", [0xe1, 0x00, 0x98])),
    ("gql", lang("GraphQL", [0xe1, 0x00, 0x98])),
    ("qml", lang("QML", [0x44, 0xa5, 0x1c])),
    ("tex", lang("TeX", [0x3d, 0x61, 0x17])),
    ("sty", lang("TeX", [0x3d, 0x61, 0x17])),
    ("ipynb", lang("Jupyter Notebook", [0xda, 0x5b, 0x0b])),
    ("sol", lang("Solidity", [0xaa, 0x67, 0x46])),
    ("hx", lang("Haxe", [0xdf, 0x79, 0x00])),
    ("cr", lang("Crystal", [0x00, 0x01, 0x00])),
    ("gd", lang("GDScript", [0x35, 0x55, 0x70])),
    ("ahk", lang("AutoHotkey", [0x65, 0x94, 0xb9])),
    ("vim", lang("Vim Script", [0x19, 0x9f, 0x4b])),
    ("applescript", lang("AppleScript", [0x10, 0x1f, 0x1f])),
    ("xaml", lang("XAML", [0x00, 0x00, 0x00])),
];

/// Languages by whole file name.
const BY_NAME: &[(&str, Language)] = &[
    ("makefile", lang("Makefile", [0x42, 0x78, 0x19])),
    ("gnumakefile", lang("Makefile", [0x42, 0x78, 0x19])),
    ("cmakelists.txt", lang("CMake", [0xda, 0x34, 0x34])),
    ("dockerfile", lang("Dockerfile", [0x38, 0x4d, 0x54])),
    ("containerfile", lang("Dockerfile", [0x38, 0x4d, 0x54])),
    ("justfile", lang("Just", [0x38, 0x4d, 0x54])),
    ("rakefile", lang("Ruby", [0x70, 0x15, 0x16])),
    ("gemfile", lang("Ruby", [0x70, 0x15, 0x16])),
    ("jenkinsfile", lang("Groovy", [0x42, 0x98, 0xb8])),
    ("meson.build", lang("Meson", [0x00, 0x78, 0x00])),
    ("build.bazel", lang("Starlark", [0x76, 0xd2, 0x75])),
    ("workspace.bazel", lang("Starlark", [0x76, 0xd2, 0x75])),
];

/// Folders whose content is third-party code.
const VENDORED_DIRS: &[&str] = &[
    "node_modules",
    "bower_components",
    "jspm_packages",
    "vendor",
    "vendors",
    "vendored",
    "third_party",
    "third-party",
    "thirdparty",
    "3rdparty",
    "3rd_party",
    "external",
    "externals",
    "extern",
    "pods",
    "carthage",
    ".yarn",
    ".venv",
    "venv",
    "site-packages",
];

/// Folders holding build output or generated code. (`build/`, `out/` and
/// `target/` are left alone: they are usually ignored, and when committed
/// they tend to hold build scripts.)
const GENERATED_DIRS: &[&str] = &[
    "dist",
    "generated",
    "__generated__",
    "gen-src",
    ".next",
    ".nuxt",
];

/// File name endings of generated code (lowercase).
const GENERATED_SUFFIXES: &[&str] = &[
    ".min.js",
    ".min.css",
    "-min.js",
    ".bundle.js",
    ".pb.go",
    ".pb.cc",
    ".pb.h",
    ".pb.swift",
    "_pb2.py",
    "_pb2_grpc.py",
    "_pb.js",
    "_pb.d.ts",
    "_grpc.pb.go",
    ".g.dart",
    ".freezed.dart",
    ".designer.cs",
    ".g.cs",
    ".generated.cs",
    "_generated.go",
    "_generated.rs",
    ".d.ts.map",
    ".js.map",
];

/// The language of a path, from its file name or extension.
#[must_use]
pub fn detect(path: &str) -> Option<Language> {
    let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    if let Some((_, l)) = BY_NAME.iter().find(|(n, _)| *n == name) {
        return Some(*l);
    }
    if name.starts_with("dockerfile.") || name.ends_with(".dockerfile") {
        return Some(lang("Dockerfile", [0x38, 0x4d, 0x54]));
    }
    let (_, ext) = name.rsplit_once('.')?;
    if ext == "h" {
        return Some(C);
    }
    BY_EXTENSION
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, l)| *l)
}

/// The color of a language named by `linguist-language` (case ignored).
#[must_use]
pub fn color_of(name: &str) -> [u8; 3] {
    BY_EXTENSION
        .iter()
        .chain(BY_NAME)
        .map(|(_, l)| l)
        .chain([&C, &CPP, &OBJC])
        .find(|l| l.name.eq_ignore_ascii_case(name))
        .map_or(UNKNOWN_COLOR, |l| l.color)
}

/// Whether a path is vendored or generated by the built-in rules.
#[must_use]
pub fn is_excluded(path: &str) -> bool {
    let lower = path.to_lowercase();
    let mut parts: Vec<&str> = lower.split('/').collect();
    let Some(name) = parts.pop() else {
        return false;
    };
    parts
        .iter()
        .any(|d| VENDORED_DIRS.contains(d) || GENERATED_DIRS.contains(d))
        || GENERATED_SUFFIXES.iter().any(|s| name.ends_with(s))
        || name.contains(".generated.")
}

/// Folders left in or out of the count, per repository, as ticked in a
/// folder tree. A file counts as its nearest folder set says (the
/// repository root is the folder `""`), and counts when none is set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderFilter {
    /// Per repository: folder → counted. Never holds a folder set the way
    /// its parent already is.
    set: BTreeMap<String, BTreeMap<String, bool>>,
}

/// How much of a folder is counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderState {
    /// Everything below it.
    All,
    /// Some subfolders, not all.
    Some,
    /// Nothing below it.
    None,
}

/// The folder holding `path` (`""` for the root and what is at the root).
fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(p, _)| p)
}

/// Whether `path` is strictly below `folder`.
fn is_below(path: &str, folder: &str) -> bool {
    if folder.is_empty() {
        !path.is_empty()
    } else {
        path.strip_prefix(folder)
            .is_some_and(|rest| rest.starts_with('/'))
    }
}

/// Whether `path` counts by the folders set in one repository.
fn keeps_in(set: Option<&BTreeMap<String, bool>>, path: &str) -> bool {
    let Some(set) = set else {
        return true;
    };
    let mut p = path;
    loop {
        if let Some(v) = set.get(p) {
            return *v;
        }
        if p.is_empty() {
            return true;
        }
        p = parent(p);
    }
}

impl FolderFilter {
    /// Whether it counts every file.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    /// Whether the file or folder at `path` in `repo` counts.
    #[must_use]
    pub fn keeps(&self, repo: &str, path: &str) -> bool {
        keeps_in(self.set.get(repo), path)
    }

    /// How much of `folder` in `repo` counts.
    #[must_use]
    pub fn state(&self, repo: &str, folder: &str) -> FolderState {
        let set = self.set.get(repo);
        let on = keeps_in(set, folder);
        let mixed = set.is_some_and(|set| set.iter().any(|(k, v)| *v != on && is_below(k, folder)));
        match (mixed, on) {
            (true, _) => FolderState::Some,
            (false, true) => FolderState::All,
            (false, false) => FolderState::None,
        }
    }

    /// Ticks `folder` in `repo` (with everything below it) when it is not
    /// wholly counted, unticks it otherwise.
    pub fn toggle(&mut self, repo: &str, folder: &str) {
        let tick = self.state(repo, folder) != FolderState::All;
        let set = self.set.entry(repo.to_owned()).or_default();
        set.retain(|k, _| k != folder && !is_below(k, folder));
        if keeps_in(Some(set), folder) != tick {
            set.insert(folder.to_owned(), tick);
        }
        if set.is_empty() {
            self.set.remove(repo);
        }
    }

    /// Counts every folder of `repo` again.
    pub fn clear(&mut self, repo: &str) {
        self.set.remove(repo);
    }
}

/// A folder holding source files, with the lines of those below it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderNode {
    /// Folder name (`""` for the root).
    pub name: String,
    /// Path in the tree (`""` for the root).
    pub path: String,
    /// Lines of the source files below it.
    pub lines: u64,
    /// Number of those files.
    pub files: u64,
    /// Subfolders holding source files, by name.
    pub children: Vec<FolderNode>,
}

#[derive(Default)]
struct FolderBuild {
    lines: u64,
    files: u64,
    children: BTreeMap<String, FolderBuild>,
}

impl FolderBuild {
    fn finish(self, name: String, path: String) -> FolderNode {
        let children = self
            .children
            .into_iter()
            .map(|(n, b)| {
                let p = if path.is_empty() {
                    n.clone()
                } else {
                    format!("{path}/{n}")
                };
                b.finish(n, p)
            })
            .collect();
        FolderNode {
            name,
            path,
            lines: self.lines,
            files: self.files,
            children,
        }
    }
}

/// The language of a file: its `linguist-language`, or else from its name.
fn language_of(f: &TreeFile) -> Option<Language> {
    match &f.language {
        Some(name) => Some(Language {
            name: BY_EXTENSION
                .iter()
                .chain(BY_NAME)
                .map(|(_, l)| l.name)
                .find(|n| n.eq_ignore_ascii_case(name))
                .unwrap_or(""),
            color: color_of(name),
        }),
        None => detect(&f.path),
    }
}

/// Whether a file is left out as vendored or generated.
fn is_left_out(f: &TreeFile) -> bool {
    f.excluded.unwrap_or_else(|| is_excluded(&f.path))
}

/// The share of one language.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageShare {
    /// Display name.
    pub name: String,
    /// Usual color (RGB).
    pub color: [u8; 3],
    /// Lines of code (all lines of its files).
    pub lines: u64,
    /// Number of files.
    pub files: u64,
}

/// The languages of one repository.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoLanguages {
    /// Repository name.
    pub repo: String,
    /// Languages, most lines first.
    pub languages: Vec<LanguageShare>,
    /// Files of a known language left out (vendored or generated).
    pub excluded_files: u64,
    /// Lines of those files.
    pub excluded_lines: u64,
    /// Files of a known language left out by the [`FolderFilter`].
    pub filtered_files: u64,
    /// Lines of those files.
    pub filtered_lines: u64,
    /// The files of the tree, to count again with another filter.
    tree: Arc<[TreeFile]>,
}

impl RepoLanguages {
    /// Lines in all the languages counted.
    #[must_use]
    pub fn lines(&self) -> u64 {
        self.languages.iter().map(|l| l.lines).sum()
    }

    /// Files in all the languages counted.
    #[must_use]
    pub fn files(&self) -> u64 {
        self.languages.iter().map(|l| l.files).sum()
    }

    /// The breakdown of the same tree, counting only the files `filter`
    /// keeps.
    #[must_use]
    pub fn filtered(&self, filter: &FolderFilter) -> Self {
        breakdown_filtered(&self.repo, &self.tree, filter)
    }

    /// The folders holding the files counted without a filter (of a known
    /// language, neither vendored nor generated), from the root.
    #[must_use]
    pub fn folders(&self) -> FolderNode {
        let mut root = FolderBuild::default();
        for f in self.tree.iter() {
            if language_of(f).is_none() || is_left_out(f) {
                continue;
            }
            let mut node = &mut root;
            node.lines += f.lines;
            node.files += 1;
            for dir in f
                .path
                .split('/')
                .rev()
                .skip(1)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                node = node.children.entry(dir.to_owned()).or_default();
                node.lines += f.lines;
                node.files += 1;
            }
        }
        root.finish(String::new(), String::new())
    }
}

/// The language breakdown of one repository's tree.
///
/// A `.h` header counts as C++ when the tree has more C++ than C sources,
/// as Objective-C when it has more Objective-C sources than both, and as C
/// otherwise.
#[must_use]
pub fn breakdown(repo: &str, files: &[TreeFile]) -> RepoLanguages {
    breakdown_filtered(repo, &Arc::from(files), &FolderFilter::default())
}

/// The language breakdown of one repository's tree, counting only the files
/// `filter` keeps (see [`breakdown`]).
fn breakdown_filtered(repo: &str, files: &Arc<[TreeFile]>, filter: &FolderFilter) -> RepoLanguages {
    let mut out = RepoLanguages {
        repo: repo.to_owned(),
        tree: files.clone(),
        ..RepoLanguages::default()
    };
    let set = filter.set.get(repo);
    let mut kept: Vec<(Language, &TreeFile)> = Vec::new();
    for f in files.iter() {
        let Some(language) = language_of(f) else {
            continue;
        };
        if is_left_out(f) {
            out.excluded_files += 1;
            out.excluded_lines += f.lines;
            continue;
        }
        if !keeps_in(set, &f.path) {
            out.filtered_files += 1;
            out.filtered_lines += f.lines;
            continue;
        }
        kept.push((language, f));
    }
    let count = |name: &str, ext: &[&str]| {
        kept.iter()
            .filter(|(l, f)| {
                l.name == name
                    && f.path
                        .rsplit_once('.')
                        .is_some_and(|(_, e)| ext.contains(&e.to_lowercase().as_str()))
            })
            .count()
    };
    let (c, cpp, objc) = (
        count("C", &["c"]),
        count("C++", &["cc", "cpp", "cxx", "c++"]),
        count("Objective-C", &["m"]) + count("Objective-C++", &["mm"]),
    );
    let header = if objc > c && objc > cpp {
        OBJC
    } else if cpp > c {
        CPP
    } else {
        C
    };
    let mut by_name: HashMap<String, LanguageShare> = HashMap::new();
    for (language, f) in kept {
        let is_header =
            f.language.is_none() && f.path.rsplit_once('.').is_some_and(|(_, e)| e == "h");
        let language = if is_header { header } else { language };
        let name = if language.name.is_empty() {
            f.language.clone().unwrap_or_default()
        } else {
            language.name.to_owned()
        };
        let share = by_name
            .entry(name.clone())
            .or_insert_with(|| LanguageShare {
                name,
                color: language.color,
                lines: 0,
                files: 0,
            });
        share.lines += f.lines;
        share.files += 1;
    }
    out.languages = by_name.into_values().collect();
    sort(&mut out.languages);
    out
}

/// Sums the languages of several repositories.
#[must_use]
pub fn merge<'a>(repos: impl IntoIterator<Item = &'a RepoLanguages>) -> Vec<LanguageShare> {
    let mut by_name: HashMap<&str, LanguageShare> = HashMap::new();
    for r in repos {
        for l in &r.languages {
            let share = by_name.entry(&l.name).or_insert_with(|| LanguageShare {
                lines: 0,
                files: 0,
                ..l.clone()
            });
            share.lines += l.lines;
            share.files += l.files;
        }
    }
    let mut out: Vec<LanguageShare> = by_name.into_values().collect();
    sort(&mut out);
    out
}

fn sort(languages: &mut [LanguageShare]) {
    languages.sort_by(|a, b| {
        b.lines
            .cmp(&a.lines)
            .then(b.files.cmp(&a.files))
            .then_with(|| a.name.cmp(&b.name))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, lines: u64) -> TreeFile {
        TreeFile {
            path: path.to_owned(),
            lines,
            ..TreeFile::default()
        }
    }

    #[test]
    fn detects_by_extension_and_name() {
        assert_eq!(detect("src/main.rs").unwrap().name, "Rust");
        assert_eq!(detect("web/App.TSX").unwrap().name, "TypeScript");
        assert_eq!(detect("Makefile").unwrap().name, "Makefile");
        assert_eq!(detect("docker/Dockerfile.dev").unwrap().name, "Dockerfile");
        assert_eq!(detect("lib/CMakeLists.txt").unwrap().name, "CMake");
        assert!(detect("README.md").is_none());
        assert!(detect("Cargo.toml").is_none());
        assert!(detect("notes.txt").is_none());
        assert!(detect("LICENSE").is_none());
    }

    #[test]
    fn excludes_vendored_and_generated() {
        assert!(is_excluded("web/node_modules/x/index.js"));
        assert!(is_excluded("third_party/zlib/inflate.c"));
        assert!(is_excluded("Vendor/lib.go"));
        assert!(is_excluded("assets/app.min.js"));
        assert!(is_excluded("api/user.pb.go"));
        assert!(is_excluded("ui/Form1.Designer.cs"));
        assert!(is_excluded("dist/bundle.js"));
        assert!(!is_excluded("src/vendor_api.rs"));
        assert!(!is_excluded("src/main.rs"));
    }

    #[test]
    fn breakdown_by_lines_and_files() {
        let files = [
            file("src/main.rs", 300),
            file("src/lib.rs", 100),
            file("build.sh", 20),
            file("README.md", 500),
            file("vendor/x/y.rs", 10_000),
            file("web/app.js", 80),
        ];
        let r = breakdown("app", &files);
        let names: Vec<&str> = r.languages.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Rust", "JavaScript", "Shell"]);
        assert_eq!(r.languages[0].lines, 400);
        assert_eq!(r.languages[0].files, 2);
        assert_eq!(r.lines(), 500);
        assert_eq!(r.files(), 4);
        assert_eq!(r.excluded_files, 1);
        assert_eq!(r.excluded_lines, 10_000);
    }

    #[test]
    fn headers_follow_the_sources() {
        let c = breakdown("c", &[file("a.c", 10), file("a.h", 5)]);
        assert_eq!(c.languages[0].name, "C");
        assert_eq!(c.languages[0].lines, 15);
        let cpp = breakdown("cpp", &[file("a.cpp", 10), file("b.cc", 1), file("a.h", 5)]);
        assert_eq!(cpp.languages.len(), 1);
        assert_eq!(cpp.languages[0].name, "C++");
        assert_eq!(cpp.languages[0].files, 3);
        let objc = breakdown("objc", &[file("a.m", 10), file("a.h", 5)]);
        assert_eq!(objc.languages[0].name, "Objective-C");
    }

    #[test]
    fn linguist_attributes_override() {
        let files = [
            TreeFile {
                excluded: Some(true),
                ..file("src/schema.rs", 900)
            },
            TreeFile {
                excluded: Some(false),
                ..file("vendor/ours.go", 40)
            },
            TreeFile {
                language: Some("python".into()),
                ..file("tools/run", 30)
            },
            TreeFile {
                language: Some("Brainfuck".into()),
                ..file("x.bf", 1)
            },
        ];
        let r = breakdown("r", &files);
        let names: Vec<&str> = r.languages.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Go", "Python", "Brainfuck"]);
        assert_eq!(r.languages[2].color, UNKNOWN_COLOR);
        assert_eq!(r.excluded_files, 1);
    }

    #[test]
    fn folder_ticks() {
        use FolderState::{All, None, Some};
        let mut f = FolderFilter::default();
        assert!(f.is_empty());
        assert!(f.keeps("app", "tests/a.rs"));
        // Untick a folder: what is below it is left out.
        f.toggle("app", "tests");
        assert!(!f.keeps("app", "tests/a.rs"));
        assert!(!f.keeps("app", "tests/deep/b.rs"));
        assert!(f.keeps("app", "tests_util.rs"));
        assert!(f.keeps("lib", "tests/a.rs"));
        assert_eq!(f.state("app", ""), Some);
        assert_eq!(f.state("app", "tests"), None);
        assert_eq!(f.state("app", "tests/deep"), None);
        assert_eq!(f.state("app", "src"), All);
        // Only src: untick the root (a partly ticked folder ticks whole
        // first), tick src back.
        f.toggle("app", "");
        assert_eq!(f.state("app", ""), All);
        f.toggle("app", "");
        assert_eq!(f.state("app", ""), None);
        f.toggle("app", "src");
        assert!(f.keeps("app", "src/main.rs"));
        assert!(!f.keeps("app", "build.rs"));
        assert!(!f.keeps("app", "tests/a.rs"));
        assert_eq!(f.state("app", ""), Some);
        // A mixed folder ticks whole; ticking back what the parent is
        // leaves nothing set.
        f.toggle("app", "");
        assert_eq!(f.state("app", ""), All);
        assert!(f.is_empty());
        f.toggle("app", "src");
        f.clear("app");
        assert!(f.is_empty());
    }

    #[test]
    fn folder_tree() {
        let files = [
            file("src/main.rs", 300),
            file("src/ui/view.rs", 20),
            file("tests/it.rs", 50),
            file("docs/guide.md", 900),
            file("vendor/x.c", 40),
            file("build.rs", 5),
        ];
        let root = breakdown("app", &files).folders();
        assert_eq!((root.lines, root.files), (375, 4));
        let names: Vec<&str> = root.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["src", "tests"]);
        let src = &root.children[0];
        assert_eq!((src.path.as_str(), src.lines, src.files), ("src", 320, 2));
        assert_eq!(src.children[0].path, "src/ui");
        assert_eq!(src.children[0].lines, 20);
    }

    #[test]
    fn filtered_breakdown() {
        let files = [
            file("src/main.rs", 300),
            file("src/a.h", 10),
            file("tests/it.rs", 50),
            file("tests/fixture.cpp", 900),
            file("vendor/x.c", 40),
            file("README.md", 500),
        ];
        let all = breakdown("app", &files);
        assert_eq!(all.lines(), 1260);
        let mut filter = FolderFilter::default();
        filter.toggle("app", "tests");
        let r = all.filtered(&filter);
        let names: Vec<&str> = r.languages.iter().map(|l| l.name.as_str()).collect();
        // The header follows the sources counted: no C++ left, so C.
        assert_eq!(names, ["Rust", "C"]);
        assert_eq!(r.lines(), 310);
        assert_eq!((r.filtered_files, r.filtered_lines), (2, 950));
        assert_eq!((r.excluded_files, r.excluded_lines), (1, 40));
        // Filters do not stack: each starts from the whole tree.
        let mut only = FolderFilter::default();
        only.toggle("app", "");
        only.toggle("app", "tests");
        let only = r.filtered(&only);
        assert_eq!(only.lines(), 950);
        assert_eq!(only.filtered(&FolderFilter::default()), all);
        // Another repository's folders change nothing.
        assert_eq!(
            all.filtered(&{
                let mut f = FolderFilter::default();
                f.toggle("other", "src");
                f
            })
            .lines(),
            all.lines()
        );
    }

    #[test]
    fn merges_repositories() {
        let a = breakdown("a", &[file("x.rs", 10), file("y.py", 5)]);
        let b = breakdown("b", &[file("z.py", 20)]);
        let all = merge([&a, &b]);
        assert_eq!(all[0].name, "Python");
        assert_eq!(all[0].lines, 25);
        assert_eq!(all[0].files, 2);
        assert_eq!(all[1].lines, 10);
    }
}
