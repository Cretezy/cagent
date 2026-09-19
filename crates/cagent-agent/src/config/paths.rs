use std::path::{Path, PathBuf};

use directories::BaseDirs;

use crate::RuntimeError;

#[derive(Clone, Debug, Default)]
pub struct PathOverrides {
    pub config_file: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppPaths {
    pub config_file: PathBuf,
    pub permissions_file: PathBuf,
    pub global_instruction_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl AppPaths {
    /// Resolves Cagent's platform paths and applies explicit overrides.
    ///
    /// # Errors
    /// Returns an error when the operating system has no discoverable base directories.
    pub fn resolve(overrides: PathOverrides) -> Result<Self, RuntimeError> {
        let defaults = if overrides.config_file.is_none() || overrides.data_dir.is_none() {
            let base = BaseDirs::new().ok_or(RuntimeError::PlatformDirectoriesUnavailable)?;
            Some(platform_directories(&base))
        } else {
            None
        };
        let (default_config_dir, default_data_dir, default_instruction_dir) =
            defaults.unwrap_or_default();
        let config_overridden = overrides.config_file.is_some();
        let config_file = overrides
            .config_file
            .unwrap_or_else(|| default_config_dir.join("config.toml"));
        let data_dir = overrides.data_dir.unwrap_or(default_data_dir);
        let global_instruction_dir = if config_overridden {
            config_file
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf()
        } else {
            default_instruction_dir
        };
        let permissions_file = config_file
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("permissions.toml");
        Ok(Self {
            config_file,
            permissions_file,
            global_instruction_dir,
            data_dir,
        })
    }

    /// Creates the configuration and data directories when absent.
    ///
    /// # Errors
    /// Returns an error if either directory cannot be created.
    pub fn create_directories(&self) -> Result<(), RuntimeError> {
        if let Some(config_dir) = self.config_file.parent() {
            std::fs::create_dir_all(config_dir)?;
        }
        std::fs::create_dir_all(&self.data_dir)?;
        Ok(())
    }
}

fn platform_directories(base: &BaseDirs) -> (PathBuf, PathBuf, PathBuf) {
    #[cfg(target_os = "macos")]
    let config_dir = base.home_dir().join(".config/cagent");
    #[cfg(not(target_os = "macos"))]
    let config_dir = base.config_dir().join("cagent");

    #[cfg(target_os = "windows")]
    let data_dir = base.data_local_dir().join("cagent");
    #[cfg(not(target_os = "windows"))]
    let data_dir = base.data_dir().join("cagent");

    let instruction_dir = config_dir.clone();
    (config_dir, data_dir, instruction_dir)
}
