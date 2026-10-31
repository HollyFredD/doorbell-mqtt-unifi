//! The local profiles directory: preset lookup and asset download folders.

use crate::config::Preset;
use crate::utils;

use chrono::{DateTime, Utc};
use std::fs;
use std::path::{Path, PathBuf};

pub struct ProfilesRepo {
    dir: PathBuf,
}

impl ProfilesRepo {
    pub fn init(base_dir: &Path) -> Option<ProfilesRepo> {
        if !utils::directory_exists(base_dir) {
            log_error!("'{}' not found or not a directory.", base_dir.display());
            return None;
        }

        log_info!(
            "Profile repository initialized with base directory '{}'.",
            base_dir.display()
        );
        Some(ProfilesRepo {
            dir: base_dir.to_path_buf(),
        })
    }

    /// Directory of the preset whose display name matches `preset_name` (case-insensitive).
    pub fn resolve_preset(&self, presets: &[Preset], preset_name: &str) -> Option<PathBuf> {
        let Some(preset) = presets
            .iter()
            .find(|p| p.display_name.eq_ignore_ascii_case(preset_name))
        else {
            log_warn!("No directory configured for preset '{}'.", preset_name);
            return None;
        };

        log_debug!(
            "Resolved preset '{}' to profile directory '{}'.",
            preset_name,
            preset.directory
        );
        Some(utils::build_path(&self.dir, &preset.directory))
    }

    /// `<profiles>/tmp/download_XXXXXX`
    pub fn create_temp_profile_dir(&self) -> Option<PathBuf> {
        let tmp = utils::build_path(&self.dir, "tmp");

        if !utils::create_directory(&tmp) {
            log_error!("Failed to create directory '{}'", tmp.display());
            return None;
        }

        let prefix = format!("{}/download_", tmp.display());
        utils::make_temp_dir(&prefix)
            .map_err(|e| {
                log_error!(
                    "Failed to create temp directory for '{}XXXXXX': {}",
                    prefix,
                    e
                )
            })
            .ok()
    }

    /// Moves a download to `<profiles>/downloads/<timestamp>` (or `partial/` when incomplete).
    /// Returns the timestamp directory name and the final path.
    pub fn rename_temp_profile_dir(
        &self,
        temp_dir: &Path,
        time: &DateTime<Utc>,
        partial: bool,
    ) -> Option<(String, PathBuf)> {
        let ts = utils::timestamp_dir(time);
        let sub = if partial { "partial" } else { "downloads" };
        let download_path = utils::build_path(&self.dir, sub);

        if !utils::create_directory(&download_path) {
            log_error!("Failed to create directory '{}'", download_path.display());
            return None;
        }

        let final_path = utils::build_path(&download_path, &ts);

        if let Err(e) = fs::rename(temp_dir, &final_path) {
            log_error!(
                "Failed to rename directory '{}' to '{}': {}",
                temp_dir.display(),
                final_path.display(),
                e
            );
            return None;
        }

        Some((ts, final_path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_presets() -> Vec<Preset> {
        vec![
            Preset {
                display_name: "Christmas".into(),
                key_name: "christmas".into(),
                directory: "christmas".into(),
            },
            Preset {
                display_name: "Labor Day".into(),
                key_name: "labor day".into(),
                directory: "labor_day".into(),
            },
        ]
    }

    #[test]
    fn init_fails_for_missing_dir() {
        assert!(ProfilesRepo::init(Path::new("/no/such/dir")).is_none());
    }

    #[test]
    fn resolve_preset_returns_profile_directory() {
        let repo = ProfilesRepo::init(Path::new("tests/fixtures/profiles")).unwrap();
        assert_eq!(
            repo.resolve_preset(&mock_presets(), "christmas"),
            Some(PathBuf::from("tests/fixtures/profiles/christmas"))
        );
        assert_eq!(repo.resolve_preset(&mock_presets(), "Easter"), None);
    }
}
