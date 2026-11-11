//! The only application-owned state root. No platform-data or cwd fallback.
use anyhow::{Context, Result, ensure};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::{
    fs::{self, DirBuilder, OpenOptions},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub root: PathBuf,
    pub database: PathBuf,
    pub audio_tmp: PathBuf,
    pub audio_retained: PathBuf,
    pub cache: PathBuf,
    pub logs: PathBuf,
    pub backups: PathBuf,
}
impl AppPaths {
    pub fn discover() -> Result<Self> {
        let home = directories::BaseDirs::new()
            .context("cannot resolve the current user's home directory")?;
        Self::from_home(home.home_dir())
    }
    /// Injectable home must exist and be absolute. Creates only `.rapid-mlx-ui` below it.
    pub fn from_home(home: &Path) -> Result<Self> {
        ensure!(
            home.is_absolute() && home.is_dir(),
            "home must be an existing absolute directory"
        );
        let root = home.join(".rapid-mlx-ui");
        let paths = Self {
            database: root.join("state.sqlite3"),
            audio_tmp: root.join("audio/tmp"),
            audio_retained: root.join("audio/retained"),
            cache: root.join("cache"),
            logs: root.join("logs"),
            backups: root.join("backups"),
            root,
        };
        paths.ensure_layout()?;
        Ok(paths)
    }
    pub(crate) fn ensure_layout(&self) -> Result<()> {
        ensure!(
            self.database == self.root.join("state.sqlite3")
                && self.audio_tmp == self.root.join("audio/tmp")
                && self.audio_retained == self.root.join("audio/retained")
                && self.cache == self.root.join("cache")
                && self.logs == self.root.join("logs")
                && self.backups == self.root.join("backups"),
            "invalid application state layout"
        );
        for dir in [
            &self.root,
            &self.root.join("audio"),
            &self.audio_tmp,
            &self.audio_retained,
            &self.cache,
            &self.logs,
            &self.backups,
        ] {
            private_dir(dir)?;
        }
        // Detect unusable roots even when the database already exists. Never relocate state.
        let probe = self
            .root
            .join(format!(".write-probe-{}", uuid::Uuid::new_v4()));
        private_file(&probe)?;
        fs::remove_file(probe)?;
        Ok(())
    }
}
pub(crate) fn private_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => ensure!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "state directory must not be a symlink or non-directory: {}",
            path.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = DirBuilder::new();
            #[cfg(unix)]
            builder.mode(0o700);
            builder.create(path).with_context(|| {
                format!("cannot create private state directory {}", path.display())
            })?;
        }
        Err(e) => return Err(e.into()),
    }
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
pub(crate) fn private_file(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            ensure!(
                meta.is_file() && !meta.file_type().is_symlink(),
                "state file must be a regular file: {}",
                path.display()
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                ensure!(meta.nlink() == 1, "state file must not be hard-linked");
                fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            options
                .open(path)
                .with_context(|| format!("cannot create private state file {}", path.display()))?;
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_layout_is_private_and_home_injected() -> Result<()> {
        let home = tempfile::tempdir()?;
        let paths = AppPaths::from_home(home.path())?;
        assert_eq!(
            paths.database,
            home.path().join(".rapid-mlx-ui/state.sqlite3")
        );
        assert!(paths.audio_tmp.is_dir());
        assert!(!paths.database.exists());
        #[cfg(unix)]
        for dir in [&paths.root, &paths.audio_tmp, &paths.backups] {
            assert_eq!(fs::metadata(dir)?.permissions().mode() & 0o777, 0o700);
        }
        Ok(())
    }
    #[test]
    fn blocked_root_never_falls_back() -> Result<()> {
        let home = tempfile::tempdir()?;
        fs::write(home.path().join(".rapid-mlx-ui"), "blocked")?;
        assert!(AppPaths::from_home(home.path()).is_err());
        assert_eq!(fs::read_dir(home.path())?.count(), 1);
        Ok(())
    }
    #[cfg(unix)]
    #[test]
    fn symlink_root_is_rejected() -> Result<()> {
        let home = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        std::os::unix::fs::symlink(outside.path(), home.path().join(".rapid-mlx-ui"))?;
        assert!(AppPaths::from_home(home.path()).is_err());
        assert_eq!(fs::read_dir(outside.path())?.count(), 0);
        Ok(())
    }
}
