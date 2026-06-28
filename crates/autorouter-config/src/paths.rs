//! Cross-platform path resolution for AutoRouter.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;

#[derive(Debug, Clone)]
pub struct ProjectPaths {
    pub data_dir: PathBuf,
    pub config_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub log_dir: PathBuf,
}

impl ProjectPaths {
    pub fn resolve() -> Option<Self> {
        let dirs = ProjectDirs::from("com", "", "autorouter")?;
        Some(Self {
            data_dir: dirs.data_dir().to_path_buf(),
            config_dir: dirs.config_dir().to_path_buf(),
            cache_dir: dirs.cache_dir().to_path_buf(),
            log_dir: dirs
                .state_dir()
                .map(|p| p.join("logs"))
                .unwrap_or_else(|| dirs.data_dir().join("logs")),
        })
    }

    pub fn under_root(root: &Path) -> Self {
        Self {
            data_dir: root.join("data"),
            config_dir: root.join("config"),
            cache_dir: root.join("cache"),
            log_dir: root.join("data").join("logs"),
        }
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        for dir in [
            &self.data_dir,
            &self.config_dir,
            &self.cache_dir,
            &self.log_dir,
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}
