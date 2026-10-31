//! last_applied.json: what was applied last, and a hash of the doorbell state it
//! produced so "Validate profile" can detect drift.

use crate::config::ApplyMethod;
use crate::errors::*;
use crate::utils;

use serde_json::json;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Animation {
    pub count: i32,
    pub duration_ms: i32,
    pub enable: bool,
    pub file: Option<String>,
    pub gui_id: Option<String>,
    pub loop_: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sound {
    pub enable: bool,
    pub file: Option<String>,
    pub repeat_times: i32,
    pub sound_state_name: Option<String>,
    pub volume: i32,
}

/// customAnimations / customSounds as reported by the doorbell over IPC.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProfileState {
    pub custom_animations: Vec<Animation>,
    pub custom_sounds: Vec<Sound>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppliedState {
    pub profile_name: String,
    pub is_preset: bool,
    pub apply_method: ApplyMethod,
    pub applied_at: i64,
    pub hash: Option<String>,
}

impl ProfileState {
    fn sort(&mut self) {
        self.custom_animations.sort_by(|a, b| {
            a.gui_id
                .as_deref()
                .unwrap_or("")
                .cmp(b.gui_id.as_deref().unwrap_or(""))
        });
        self.custom_sounds.sort_by(|a, b| {
            a.sound_state_name
                .as_deref()
                .unwrap_or("")
                .cmp(b.sound_state_name.as_deref().unwrap_or(""))
        });
    }

    /// SHA-256 over a canonical line per entry. Same input format as the C service,
    /// so hashes saved by older versions still validate.
    fn compute_hash(&self) -> String {
        let b = |v: bool| if v { "1" } else { "0" };
        let mut text = String::new();

        for a in &self.custom_animations {
            let _ = writeln!(
                text,
                "A|{}|{}|{}|{}|{}|{}",
                a.gui_id.as_deref().unwrap_or(""),
                a.file.as_deref().unwrap_or(""),
                a.count,
                a.duration_ms,
                b(a.enable),
                b(a.loop_)
            );
        }

        for s in &self.custom_sounds {
            let _ = writeln!(
                text,
                "S|{}|{}|{}|{}|{}",
                s.sound_state_name.as_deref().unwrap_or(""),
                s.file.as_deref().unwrap_or(""),
                s.repeat_times,
                s.volume,
                b(s.enable)
            );
        }

        format!("{:x}", Sha256::digest(text.as_bytes()))
    }
}

fn last_applied_path(state_dir: &Path) -> std::path::PathBuf {
    utils::build_path(state_dir, "last_applied.json")
}

pub fn load(state_dir: &Path) -> AppResult<AppliedState> {
    let path = last_applied_path(state_dir);

    let text = fs::read_to_string(&path).map_err(|e| {
        log_error!("Failed to read the state file: {}: {}", path.display(), e);
        AppError::from_code(ERROR_STATE_FILE_READ_FAILED)
    })?;

    let root = utils::parse_json_lenient(&text).map_err(|e| {
        log_error!("JSON parsing error in {}: {}", path.display(), e);
        AppError::from_code(ERROR_STATE_PARSE_FAILED)
    })?;

    let profile_name = utils::json_str(&root, "profileName")
        .filter(|n| !n.is_empty())
        .ok_or_else(|| AppError::from_code(ERROR_STATE_PARSE_FAILED))?;

    let apply_method = match utils::json_str(&root, "applyMethod") {
        Some(m) if m.eq_ignore_ascii_case("ipc") => ApplyMethod::Ipc,
        _ => ApplyMethod::Legacy,
    };

    Ok(AppliedState {
        profile_name: profile_name.to_string(),
        is_preset: utils::json_bool(&root, "isPreset").unwrap_or(false),
        apply_method,
        applied_at: utils::json_f64(&root, "appliedAt").map_or(0, |t| t as i64),
        hash: utils::json_str(&root, "hash").map(str::to_string),
    })
}

/// Writes last_applied.json. With IPC the hash of `profile_state` is stored for later validation.
pub fn save(
    profile_state: &mut ProfileState,
    profile_name: &str,
    is_preset: bool,
    apply_method: ApplyMethod,
    state_dir: &Path,
) -> AppResult {
    let hash = if apply_method == ApplyMethod::Ipc {
        profile_state.sort();
        profile_state.compute_hash()
    } else {
        String::new()
    };

    let root = json!({
        "schemaVersion": 1,
        "profileName": profile_name,
        "isPreset": is_preset,
        "applyMethod": if apply_method == ApplyMethod::Ipc { "ipc" } else { "legacy" },
        "appliedAt": chrono::Utc::now().timestamp(),
        "hash": hash,
    });

    let path = last_applied_path(state_dir);
    let written = utils::create_directory(state_dir)
        && fs::write(
            &path,
            serde_json::to_string_pretty(&root).unwrap_or_default(),
        )
        .map_err(|e| log_error!("Failed to write '{}': {}", path.display(), e))
        .is_ok();

    if !written {
        log_warn!("Failed to write 'last_applied.json'; validation and state will be unavailable");
        return Err(AppError::from_code(ERROR_STATE_FILE_WRITE_FAILED));
    }

    Ok(())
}

#[derive(Debug)]
pub struct Comparison {
    pub applied: AppliedState,
    pub active_hash: String,
    pub matches: bool,
}

/// Compares the doorbell's current state with the last applied one. Err when no
/// comparison is possible (no state file, legacy apply, missing hash).
pub fn compare(active_state: &mut ProfileState, state_dir: &Path) -> AppResult<Comparison> {
    let applied = load(state_dir)?;

    if applied.apply_method == ApplyMethod::Legacy {
        return Err(AppError::from_code(ERROR_STATE_VALIDATION_UNAVAILABLE));
    }

    let Some(expected) = applied.hash.clone().filter(|h| !h.is_empty()) else {
        return Err(AppError::from_code(ERROR_STATE_HASH_MISSING));
    };

    active_state.sort();
    let active_hash = active_state.compute_hash();
    let matches = active_hash == expected;

    Ok(Comparison {
        applied,
        active_hash,
        matches,
    })
}

/// Older versions kept last_applied.json under <profiles>/.state/.
pub fn migrate_last_applied(profiles_dir: &Path, state_dir: &Path) {
    let old_path = utils::build_path(profiles_dir, ".state/last_applied.json");
    let new_path = last_applied_path(state_dir);

    if new_path.exists() || !old_path.exists() {
        return;
    }

    if !utils::create_directory(state_dir) {
        log_error!("Failed to create directory '{}'", state_dir.display());
        return;
    }

    match utils::move_file_cross_fs(&old_path, &new_path) {
        Ok(()) => log_info!(
            "Migrated last_applied state: {} -> {}",
            old_path.display(),
            new_path.display()
        ),
        Err(e) => log_warn!(
            "Failed migrating last_applied state: {} -> {} ({})",
            old_path.display(),
            new_path.display(),
            e
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ProfileState {
        ProfileState {
            custom_animations: vec![Animation {
                count: 60,
                duration_ms: 1960,
                enable: true,
                file: Some("test.png".into()),
                gui_id: Some("WELCOME".into()),
                loop_: false,
            }],
            custom_sounds: vec![Sound {
                enable: true,
                file: Some("test.ogg".into()),
                repeat_times: 1,
                sound_state_name: Some("RING_BUTTON_PRESSED".into()),
                volume: 100,
            }],
        }
    }

    #[test]
    fn hash_matches_c_format() {
        // sha256 of "A|WELCOME|test.png|60|1960|1|0\nS|RING_BUTTON_PRESSED|test.ogg|1|100|1\n"
        let expected = format!(
            "{:x}",
            Sha256::digest(
                b"A|WELCOME|test.png|60|1960|1|0\nS|RING_BUTTON_PRESSED|test.ogg|1|100|1\n"
            )
        );
        assert_eq!(sample().compute_hash(), expected);
    }

    #[test]
    fn save_then_compare_round_trip() {
        let dir = std::env::temp_dir().join(format!("doorbell-state-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);

        save(&mut sample(), "Christmas", true, ApplyMethod::Ipc, &dir).unwrap();

        let applied = load(&dir).unwrap();
        assert_eq!(applied.profile_name, "Christmas");
        assert!(applied.is_preset);

        let mut active = sample();
        assert!(compare(&mut active, &dir).unwrap().matches);

        active.custom_sounds[0].volume = 50;
        assert!(!compare(&mut active, &dir).unwrap().matches);

        save(&mut sample(), "Christmas", true, ApplyMethod::Legacy, &dir).unwrap();
        assert_eq!(
            compare(&mut sample(), &dir).unwrap_err().code,
            ERROR_STATE_VALIDATION_UNAVAILABLE
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
