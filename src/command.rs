//! Handlers for the Home Assistant command topics. Each one reports progress on
//! `status` and ends in "idle", errors go to `last_error`.

use crate::config::{ApplyMethod, Config};
use crate::errors::*;
use crate::ha::Ha;
use crate::profile;
use crate::remote;
use crate::repo::ProfilesRepo;
use crate::ssh::SshSession;
use crate::state;
use crate::utils;

use std::path::{Path, PathBuf};

pub struct Ctx {
    pub cfg: Config,
    pub ha: Ha,
    pub repo: ProfilesRepo,
    pub state_dir: PathBuf,
    pub sounds_dir: PathBuf,
}

pub type Handler = fn(&Ctx, &str);

fn connect(ctx: &Ctx) -> Option<SshSession> {
    let session = SshSession::connect(&ctx.cfg.ssh);
    if session.is_none() {
        ctx.ha
            .err(ERROR_SSH_CONNECTION_FAILED, "Failed to create SSH session");
    }
    session
}

/// Upload + apply + persist last_applied.json. Errors are reported; true on success.
fn apply_profile(
    ctx: &Ctx,
    profile_dir: &Path,
    load_error: &str,
    state_name: &str,
    is_preset: bool,
) -> bool {
    let Some(p) = profile::load_from_dir(profile_dir) else {
        ctx.ha.set_error(ERROR_PROFILE_INVALID, load_error);
        return false;
    };

    let Some(session) = connect(ctx) else {
        return false;
    };
    let method = ctx.cfg.apply_method;

    let ipc_raw = match remote::profile_upload_and_apply(&session, profile_dir, &p, method) {
        Ok(raw) => raw,
        Err(e) => {
            ctx.ha.app_err(&e);
            return false;
        }
    };

    let mut profile_state = state::ProfileState::default();
    if method == ApplyMethod::Ipc {
        match ipc_raw.as_ref().and_then(profile::parse_apply_response) {
            Some(s) => profile_state = s,
            None => {
                ctx.ha.err(
                    ERROR_STATE_PARSE_FAILED,
                    "Failed to parse IPC state response. State will not survive restart of service",
                );
                return false;
            }
        }
    }

    if let Err(e) = state::save(
        &mut profile_state,
        state_name,
        is_preset,
        method,
        &ctx.state_dir,
    ) {
        ctx.ha.set_error(
            e.code,
            &format!("Failed to persist last_applied.json (profile={state_name}). State will not survive restart of service."),
        );
    } else {
        ctx.ha.clear_error();
    }

    true
}

pub fn set_preset(ctx: &Ctx, payload: &str) {
    if payload.is_empty() {
        return;
    }

    ctx.ha.set_state("uploading");

    let ok = match ctx.repo.resolve_preset(&ctx.cfg.presets, payload) {
        Some(dir) => apply_profile(
            ctx,
            &dir,
            &format!("Error loading profile.json for preset '{payload}'"),
            payload,
            true,
        ),
        None => {
            ctx.ha.set_error(
                ERROR_PROFILE_NOT_FOUND,
                &format!("Profile directory for preset '{payload}' not found"),
            );
            false
        }
    };

    if ok {
        ctx.ha.set_last_applied_profile(payload);
        ctx.ha.set_preset_selected(payload);
        ctx.ha.set_custom_directory("");
    }

    ctx.ha.set_state("idle");
}

pub fn apply_custom(ctx: &Ctx, payload: &str) {
    if payload.is_empty() {
        return;
    }

    ctx.ha.set_state("uploading");

    let dir = utils::build_path("./profiles", payload);

    let ok = if !utils::directory_exists(&dir) {
        ctx.ha.set_error(
            ERROR_PROFILE_NOT_FOUND,
            &format!("Custom profile '{payload}' not found"),
        );
        false
    } else {
        apply_profile(
            ctx,
            &dir,
            &format!("Error loading profile.json for '{payload}'"),
            payload,
            false,
        )
    };

    if ok {
        ctx.ha.set_last_applied_profile(payload);
        ctx.ha.set_preset_selected("none");
    }

    ctx.ha.set_state("idle");
}

/// Applies an explicit profile directory (used by the embedded web UI).
/// Unlike `apply_custom` the directory is resolved against the configured
/// profile repository, so it works from any working directory.
pub fn apply_profile_dir(ctx: &Ctx, dir: &Path, dir_label: &str) {
    ctx.ha.set_state("uploading");

    if !utils::directory_exists(dir) {
        ctx.ha.set_error(
            ERROR_PROFILE_NOT_FOUND,
            &format!("Profile '{dir_label}' not found"),
        );
        ctx.ha.set_state("idle");
        return;
    }

    if apply_profile(
        ctx,
        dir,
        &format!("Error loading profile.json for '{dir_label}'"),
        dir_label,
        false,
    ) {
        ctx.ha.set_last_applied_profile(dir_label);
        ctx.ha.set_preset_selected("none");
        ctx.ha.set_custom_directory(dir_label);
    }

    ctx.ha.set_state("idle");
}

/// Plays an ad-hoc sound file from the sounds directory (used by the embedded web UI).
pub fn play_sound_file(ctx: &Ctx, file: &str, volume: i32) {
    ctx.ha.set_state("uploading");

    let sfx = crate::config::SfxPreset {
        name: file.to_string(),
        key_name: file.to_string(),
        file: file.to_string(),
        volume,
    };

    if let Some(session) = connect(ctx) {
        match remote::sfx_upload_and_play(&session, &sfx, &ctx.sounds_dir) {
            Ok(()) => ctx.ha.clear_error(),
            Err(e) => ctx.ha.app_err(&e),
        }
    }

    ctx.ha.set_state("idle");
}

pub fn test_config(ctx: &Ctx, payload: &str) {
    if payload.is_empty() {
        return;
    }

    ctx.ha.set_state("uploading");

    if apply_profile(
        ctx,
        Path::new("./test-profile"),
        "Error loading profile.json for test",
        "test-config",
        false,
    ) {
        ctx.ha.set_last_applied_profile("test-config");
        ctx.ha.set_preset_selected("none");
        ctx.ha.set_custom_directory("");
    }

    ctx.ha.set_state("idle");
}

pub fn download_assets(ctx: &Ctx, payload: &str) {
    if payload.is_empty() {
        return;
    }

    ctx.ha.set_state("downloading");

    let now = chrono::Utc::now();

    let Some(temp_dir) = ctx.repo.create_temp_profile_dir() else {
        ctx.ha
            .err(ERROR_PROFILE_DOWNLOAD_FAILED, "Failed to create temp path");
        ctx.ha.set_state("idle");
        return;
    };

    let downloaded = (|| {
        let session = connect(ctx)?;

        let Some(p) = remote::profile_download_and_load(&session, &temp_dir) else {
            ctx.ha.err(
                ERROR_PROFILE_DOWNLOAD_FAILED,
                "Failed to download profile assets",
            );
            return None;
        };

        let profile_json = utils::build_path(&temp_dir, "profile.json");
        if !profile::write_to_file(&profile_json, &p) {
            ctx.ha.set_error(
                ERROR_PROFILE_DOWNLOAD_FAILED,
                &format!("Failed to write {}", profile_json.display()),
            );
            return None;
        }

        Some(())
    })()
    .is_some();

    match ctx
        .repo
        .rename_temp_profile_dir(&temp_dir, &now, !downloaded)
    {
        Some((final_dir, final_path)) => {
            if downloaded {
                ctx.ha.clear_error();
            }
            ctx.ha.set_last_download(
                &final_dir,
                &final_path.to_string_lossy(),
                &utils::iso_timestamp(&now),
            );
        }
        None => ctx.ha.err(
            ERROR_PROFILE_DOWNLOAD_FAILED,
            "Failed to rename download temp path",
        ),
    }

    ctx.ha.set_state("idle");
}

pub fn play_sfx_preset(ctx: &Ctx, payload: &str) {
    ctx.ha.set_sfx_preset_selected(payload);
    ctx.ha.set_state("uploading");

    match ctx
        .cfg
        .sfx_presets
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(payload))
    {
        None => ctx.ha.set_error(
            ERROR_SFX_PRESET_NOT_FOUND,
            &format!("SFX preset '{payload}' was not found"),
        ),
        Some(sfx) => {
            if let Some(session) = connect(ctx) {
                match remote::sfx_upload_and_play(&session, sfx, &ctx.sounds_dir) {
                    Ok(()) => ctx.ha.clear_error(),
                    Err(e) => ctx.ha.app_err(&e),
                }
            }
        }
    }

    ctx.ha.set_sfx_preset_selected("none");
    ctx.ha.set_state("idle");
}

pub fn validate_profile(ctx: &Ctx, _payload: &str) {
    ctx.ha.set_state("validating");

    let ts = utils::iso_timestamp(&chrono::Utc::now());

    if ctx.cfg.apply_method == ApplyMethod::Legacy {
        ctx.ha.set_state("idle");
        ctx.ha.set_validate_profile("unsupported", "", "", "", &ts);
        return;
    }

    if let Some(session) = connect(ctx) {
        match remote::fetch_state(&session) {
            Err(e) => {
                ctx.ha.err(e.code, "Failed to fetch active IPC state");
                ctx.ha.set_validate_profile("unknown", "", "", "", &ts);
            }
            Ok(raw) => match profile::parse_validate_response(&raw) {
                None => ctx
                    .ha
                    .err(ERROR_STATE_PARSE_FAILED, "Failed to parse IPC response"),
                Some(mut active) => match state::compare(&mut active, &ctx.state_dir) {
                    Ok(c) => {
                        let expected = c.applied.hash.as_deref().unwrap_or("");
                        let result = if c.matches { "match" } else { "mismatch" };
                        ctx.ha.set_validate_profile(
                            result,
                            &c.applied.profile_name,
                            expected,
                            &c.active_hash,
                            &ts,
                        );
                    }
                    Err(_) => ctx.ha.set_validate_profile("unknown", "", "", "", &ts),
                },
            },
        }
    }

    ctx.ha.set_state("idle");
}

pub fn reapply_last_profile(ctx: &Ctx, _payload: &str) {
    ctx.ha.set_state("validating");

    let last = match state::load(&ctx.state_dir) {
        Ok(last) => last,
        Err(e) => {
            ctx.ha.err(e.code, "Failed to load last_applied.json");
            ctx.ha.set_state("idle");
            return;
        }
    };

    if last.is_preset {
        set_preset(ctx, &last.profile_name);
    } else if last.profile_name.eq_ignore_ascii_case("test-config") {
        test_config(ctx, &last.profile_name);
    } else {
        apply_custom(ctx, &last.profile_name);
    }
}
