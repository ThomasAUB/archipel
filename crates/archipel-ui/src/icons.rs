//! The app's assets: gpui-kit's default icon bundle, plus the Lucide icons
//! of the repo actions that bundle leaves out (embedding the whole catalog
//! would add some 1800 icons for a dozen).

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        Archive,
        ArchiveRestore,
        ArrowDownToLine,
        ArrowUpFromLine,
        CloudDownload,
        FolderTree,
        GitBranchPlus,
        GitPullRequest,
        GitPullRequestCreate,
    ]
);

/// The default bundle and [`ExtraIcons`].
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match ExtraIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::assets::IconName;

    use super::*;

    #[test]
    fn repo_action_icons_load() {
        for icon in [
            IconName::Archive,
            IconName::ArchiveRestore,
            IconName::ArrowDownToLine,
            IconName::ArrowUpFromLine,
            IconName::CloudDownload,
            IconName::FolderTree,
            IconName::GitBranchPlus,
            IconName::GitPullRequest,
            IconName::GitPullRequestCreate,
            IconName::Undo2,
            IconName::Redo2,
        ] {
            let path = icon.path();
            assert!(AppAssets.load(&path).unwrap().is_some(), "{path}");
        }
    }
}
