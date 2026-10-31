//! Operations on the doorbell over SSH: upload and apply a profile, play a sound,
//! read the active state, download the current assets.

use crate::config::{ApplyMethod, SfxPreset};
use crate::errors::*;
use crate::profile::{self, IpcRaw, Profile};
use crate::ssh::{self, SshSession};
use crate::utils::{self, FileClass};

use std::fs;
use std::path::{Path, PathBuf};

const LOCAL_TMP_ROOT: &str = "/tmp/doorbell-mqtt-unifi";
const REMOTE_PROFILE_TMP: &str = "/tmp/doorbell-mqtt-unifi/profile";
const REMOTE_SFX_TMP: &str = "/tmp/doorbell-mqtt-unifi/sfx";

/// A local temp dir and a remote one, both cleaned on drop.
struct Workdir<'a> {
    session: &'a SshSession,
    remote: &'static str,
    local: PathBuf,
}

impl<'a> Workdir<'a> {
    fn prepare(session: &'a SshSession, remote: &'static str) -> AppResult<Workdir<'a>> {
        let tmp_err = || {
            AppError::new(
                ERROR_LOCAL_TEMP_DIR_CREATE_FAILED,
                "Error creating temp directories",
            )
        };

        if !utils::create_directory(Path::new(LOCAL_TMP_ROOT)) {
            log_error!("Failed to create {} directory", LOCAL_TMP_ROOT);
            return Err(tmp_err());
        }

        let local = utils::make_temp_dir(&format!("{LOCAL_TMP_ROOT}/upload-")).map_err(|e| {
            log_error!(
                "Failed to create temp directory under '{}': {}",
                LOCAL_TMP_ROOT,
                e
            );
            tmp_err()
        })?;

        let wd = Workdir {
            session,
            remote,
            local,
        };

        if !session.run(&ssh::cmd_reset_dir(remote)) {
            return Err(AppError::new(
                ERROR_SSH_COMMAND_FAILED,
                "Error creating temp directories",
            ));
        }

        Ok(wd)
    }
}

impl Drop for Workdir<'_> {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_dir_all(&self.local) {
            log_warn!("Failed to delete '{}': {}", self.local.display(), e);
        }

        if !self.session.run(&ssh::cmd_reset_dir(self.remote)) {
            log_warn!("Failed to reset '{}'", self.remote);
        }
    }
}

fn map_apply_step_to_error(step: &str, rc: i32) -> i32 {
    match (step, rc) {
        (_, 13) => ERROR_PROFILE_APPLY_PERMISSION_DENIED,
        (_, 28) => ERROR_REMOTE_DISK_FULL,
        (s, _) if s.starts_with("move_") => ERROR_PROFILE_APPLY_FILE_OPERATION_FAILED,
        (s, _) if s.starts_with("verify_") => ERROR_PROFILE_APPLY_VERIFY_FAILED,
        ("restart", _) => ERROR_PROFILE_APPLY_RESTART_FAILED,
        _ => ERROR_PROFILE_APPLY_FAILED,
    }
}

/// Runs one of the step scripts; a failing step is mapped to a specific error code.
fn run_script(session: &SshSession, script: &str, what: &str) -> AppResult {
    let out = session
        .exec(script)
        .ok_or_else(|| AppError::from_code(ERROR_PROFILE_APPLY_FAILED))?;

    if out.status == 0 {
        return Ok(());
    }

    match ssh::parse_step_error(&out.stderr) {
        Some((step, rc)) => {
            log_error!(
                "{} failed at step '{}' with return code '{}'",
                what,
                step,
                rc
            );
            Err(AppError::new(
                map_apply_step_to_error(&step, rc),
                format!("{what} failed at step '{step}' (rc={rc})"),
            ))
        }
        None => {
            log_error!(
                "{} failed (exit={}): {}",
                what,
                out.status,
                out.stderr.trim()
            );
            Err(AppError::from_code(ERROR_PROFILE_APPLY_FAILED))
        }
    }
}

const LCM_GUI_CONF: &str = "/etc/persistent/ubnt_lcm_gui.conf";
const SOUNDS_LEDS_CONF: &str = "/etc/persistent/ubnt_sounds_leds.conf";

fn conf_download(session: &SshSession, tmp_dir: &Path) -> bool {
    session.download(
        LCM_GUI_CONF,
        &utils::build_path(tmp_dir, "ubnt_lcm_gui.conf"),
    ) && session.download(
        SOUNDS_LEDS_CONF,
        &utils::build_path(tmp_dir, "ubnt_sounds_leds.conf"),
    )
}

fn check_persistent_storage(session: &SshSession) -> AppResult {
    let out = session
        .exec(ssh::SCRIPT_STORAGE_GUARDRAIL)
        .ok_or_else(|| AppError::new(ERROR_SSH_COMMAND_FAILED, "Failed to execute SSH command"))?;

    if !out.stdout.is_empty() {
        log_debug!("Storage guardrail output:\n{}", out.stdout);
    }

    if !out.stderr.is_empty() {
        log_error!("{}", out.stderr);
    }

    match out.status {
        0 => Ok(()),
        10 => {
            log_warn!("Doorbell storage guardrail warning: persistent storage nearly full.");
            Ok(())
        }
        1 => {
            log_error!("Storage guardrail failed: insufficient persistent storage.");
            Err(AppError::new(
                ERROR_REMOTE_DISK_FULL,
                "Storage guardrail failed: insufficient persistent storage.",
            ))
        }
        status => {
            log_error!("Unexpected storage guardrail exit status: {}", status);
            Err(AppError::new(
                ERROR_SSH_COMMAND_FAILED,
                format!("Unexpected storage guardrail exit status: {status}"),
            ))
        }
    }
}

/// Validates one asset, writes its .md5 next to it in the local workdir and uploads both
/// to `<remote>/anim` or `<remote>/sound`.
fn stage_single_asset(
    wd: &Workdir,
    profile_dir: &Path,
    asset_filename: &str,
    cls: FileClass,
) -> AppResult {
    utils::is_valid_filename(asset_filename, cls)?;

    let asset_path = utils::build_path(profile_dir, asset_filename);

    if !utils::file_exists(&asset_path) {
        log_error!("File '{}' does not exist", asset_path.display());
        return Err(AppError::new(
            ERROR_FILE_NOT_FOUND,
            format!("File {asset_filename} does not exist"),
        ));
    }

    utils::is_valid_file(&asset_path, cls).map_err(|e| {
        e.wrap(format!(
            "Profile asset '{asset_filename}' validation failed"
        ))
    })?;

    let md5_path = utils::build_path(&wd.local, &format!("{asset_filename}.md5"));
    let md5_written = utils::md5_file_hex(&asset_path).and_then(|hex| fs::write(&md5_path, hex));
    if let Err(e) = md5_written {
        log_error!("Failed to create MD5 hash for '{}': {}", asset_filename, e);
        return Err(AppError::new(
            ERROR_PROFILE_UPLOAD_FAILED,
            format!("Failed to create MD5 hash for {asset_filename}"),
        ));
    }

    let remote_dir = format!(
        "{}/{}",
        wd.remote,
        if cls == FileClass::Animation {
            "anim"
        } else {
            "sound"
        }
    );

    if !wd.session.run(&ssh::cmd_mkdir(&remote_dir)) {
        return Err(AppError::new(
            ERROR_SSH_COMMAND_FAILED,
            "Failed to execute SSH command",
        ));
    }

    if !wd.session.upload(&asset_path, &remote_dir, 0o644) {
        return Err(AppError::new(
            ERROR_PROFILE_UPLOAD_TRANSFER_FAILED,
            format!("Failed to upload {asset_filename}"),
        ));
    }

    if !wd.session.upload(&md5_path, &remote_dir, 0o644) {
        return Err(AppError::new(
            ERROR_PROFILE_UPLOAD_TRANSFER_FAILED,
            format!("Failed to upload MD5 file for {asset_filename}"),
        ));
    }

    Ok(())
}

fn stage_assets(wd: &Workdir, profile_dir: &Path, p: &Profile) -> AppResult {
    if p.welcome.enabled {
        stage_single_asset(wd, profile_dir, &p.welcome.file, FileClass::Animation)?;
    }

    if p.ring_button.enabled {
        stage_single_asset(wd, profile_dir, &p.ring_button.file, FileClass::Sound)?;
    }

    Ok(())
}

/// Legacy: download both .conf files, patch them locally, upload the `.patched` copies.
fn legacy_stage_artifacts(wd: &Workdir, p: &Profile) -> AppResult {
    let fail = |msg: &str| {
        log_error!("{}", msg);
        Err(AppError::new(ERROR_PROFILE_DOWNLOAD_FAILED, msg))
    };

    if !conf_download(wd.session, &wd.local) {
        return fail("Failed to download doorbell configuration");
    }

    let lcm_out = utils::build_path(&wd.local, "ubnt_lcm_gui.conf.patched");
    if !profile::patch_lcm_gui_conf(
        &utils::build_path(&wd.local, "ubnt_lcm_gui.conf"),
        &lcm_out,
        p,
    ) {
        return fail("Failed to patch ubnt_lcm_gui.conf");
    }

    if !wd.session.upload(&lcm_out, wd.remote, 0o644) {
        return Err(AppError::from_code(ERROR_PROFILE_UPLOAD_TRANSFER_FAILED));
    }

    let sounds_out = utils::build_path(&wd.local, "ubnt_sounds_leds.conf.patched");
    if !profile::patch_sounds_leds_conf(
        &utils::build_path(&wd.local, "ubnt_sounds_leds.conf"),
        &sounds_out,
        p,
    ) {
        return fail("Failed to patch ubnt_sounds_leds.conf");
    }

    if !wd.session.upload(&sounds_out, wd.remote, 0o644) {
        return Err(AppError::from_code(ERROR_PROFILE_UPLOAD_TRANSFER_FAILED));
    }

    Ok(())
}

/// IPC: build and upload the two IPC messages.
fn ipc_stage_artifacts(wd: &Workdir, p: &Profile) -> AppResult {
    type WriteMessage = fn(&Path, &Profile) -> bool;
    let messages: [(&str, WriteMessage); 2] = [
        ("lcm_ipc_msg.json", profile::write_ipc_lcm_gui_message),
        (
            "sounds_ipc_msg.json",
            profile::write_ipc_sounds_leds_message,
        ),
    ];

    for (name, write) in messages {
        let path = utils::build_path(&wd.local, name);

        if !write(&path, p) {
            log_error!("Failed to create '{}'", path.display());
            return Err(AppError::new(
                ERROR_PROFILE_UPLOAD_FAILED,
                format!("Failed to create {name}"),
            ));
        }

        if !wd.session.upload(&path, wd.remote, 0o644) {
            log_error!("Failed to upload '{}'", path.display());
            return Err(AppError::new(
                ERROR_PROFILE_UPLOAD_TRANSFER_FAILED,
                format!("Failed to upload {name}"),
            ));
        }
    }

    Ok(())
}

fn legacy_apply(wd: &Workdir) -> AppResult {
    let script = ssh::build_apply_profile_command(wd.remote)
        .ok_or_else(|| AppError::from_code(ERROR_PROFILE_APPLY_FAILED))?;
    run_script(wd.session, &script, "Apply profile")
}

/// Two ubnt_ipc_cli calls, each returning stdout or an error.
fn ipc_cli_pair(
    session: &SshSession,
    lcm_cmd: &str,
    sound_cmd: &str,
    failed: i32,
) -> AppResult<IpcRaw> {
    let run = |cmd: &str| session.exec(cmd).ok_or_else(|| AppError::from_code(failed));

    let lcm = run(lcm_cmd)?;
    let sound = run(sound_cmd)?;

    if lcm.status != 0 || sound.status != 0 {
        log_error!(
            "ubnt_ipc_cli failed (lcm exit={}, sounds exit={})",
            lcm.status,
            sound.status
        );
        return Err(AppError::from_code(ERROR_PROFILE_APPLY_FAILED));
    }

    Ok(IpcRaw {
        lcm_gui_json: lcm.stdout,
        sounds_leds_json: sound.stdout,
    })
}

fn ipc_apply(wd: &Workdir) -> AppResult<IpcRaw> {
    let script = ssh::build_move_assets_ipc(wd.remote)
        .ok_or_else(|| AppError::from_code(ERROR_PROFILE_APPLY_FAILED))?;
    run_script(wd.session, &script, "Move assets IPC")?;

    ipc_cli_pair(
        wd.session,
        &ssh::cmd_ipc_cli("ubnt_lcm_gui", &format!("{}/lcm_ipc_msg.json", wd.remote)),
        &ssh::cmd_ipc_cli(
            "ubnt_sounds_leds",
            &format!("{}/sounds_ipc_msg.json", wd.remote),
        ),
        ERROR_PROFILE_APPLY_FAILED,
    )
}

/// Uploads the profile assets and applies them. With IPC, returns the doorbell's responses.
pub fn profile_upload_and_apply(
    session: &SshSession,
    profile_dir: &Path,
    p: &Profile,
    method: ApplyMethod,
) -> AppResult<Option<IpcRaw>> {
    let wd = Workdir::prepare(session, REMOTE_PROFILE_TMP)?;

    stage_assets(&wd, profile_dir, p)?;
    check_persistent_storage(session)?;

    match method {
        ApplyMethod::Ipc => {
            ipc_stage_artifacts(&wd, p)?;
            ipc_apply(&wd).map(Some)
        }
        ApplyMethod::Legacy => {
            legacy_stage_artifacts(&wd, p)?;
            legacy_apply(&wd).map(|()| None)
        }
    }
}

pub fn sfx_upload_and_play(session: &SshSession, sfx: &SfxPreset, sounds_dir: &Path) -> AppResult {
    let wd = Workdir::prepare(session, REMOTE_SFX_TMP)?;

    let local_path = utils::build_path(sounds_dir, &sfx.file);
    utils::is_valid_file(&local_path, FileClass::Sound)
        .map_err(|e| e.wrap(format!("Sound '{}' validation failed", sfx.file)))?;

    if !session.upload(&local_path, wd.remote, 0o644) {
        log_error!("Failed to upload '{}'", local_path.display());
        return Err(AppError::new(
            ERROR_PROFILE_UPLOAD_TRANSFER_FAILED,
            format!("Failed to upload {}", sfx.file),
        ));
    }

    let remote_path = format!("{}/{}", wd.remote, sfx.file);
    if !session.run(&ssh::cmd_play_sfx(&remote_path, sfx.volume)) {
        return Err(AppError::new(
            ERROR_SFX_PLAY_COMMAND_FAILED,
            format!("Failed to play {}", sfx.file),
        ));
    }

    Ok(())
}

/// Current customAnimations / customSounds of the doorbell, read over IPC.
pub fn fetch_state(session: &SshSession) -> AppResult<IpcRaw> {
    ipc_cli_pair(
        session,
        &ssh::cmd_ipc_cli_cfg("ubnt_lcm_gui", "ChangeLcmGuiSettings", "customAnimations"),
        &ssh::cmd_ipc_cli_cfg("ubnt_sounds_leds", "ChangeSoundLedSettings", "customSounds"),
        ERROR_SSH_COMMAND_FAILED,
    )
}

/// Downloads the doorbell's .conf files and the assets they reference into `tmp_dir`.
pub fn profile_download_and_load(session: &SshSession, tmp_dir: &Path) -> Option<Profile> {
    if !conf_download(session, tmp_dir) {
        return None;
    }

    let mut p = Profile::default();

    let lcm_conf = utils::build_path(tmp_dir, "ubnt_lcm_gui.conf");
    if !profile::read_from_lcm_gui_conf(&lcm_conf, &mut p) {
        log_error!("Error loading '{}' into profile", lcm_conf.display());
        return None;
    }

    let sounds_conf = utils::build_path(tmp_dir, "ubnt_sounds_leds.conf");
    if !profile::read_from_sounds_leds_conf(&sounds_conf, &mut p) {
        log_error!("Error loading '{}' into profile", sounds_conf.display());
        return None;
    }

    if !p.welcome.file.is_empty() {
        let remote = format!("/etc/persistent/lcm/animation/{}.anim", p.welcome.file);
        if !session.download(&remote, &utils::build_path(tmp_dir, &p.welcome.file)) {
            return None;
        }
    }

    if !p.ring_button.file.is_empty() {
        let remote = format!("/etc/persistent/sounds/{}", p.ring_button.file);
        if !session.download(&remote, &utils::build_path(tmp_dir, &p.ring_button.file)) {
            return None;
        }
    }

    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_step_errors() {
        assert_eq!(
            map_apply_step_to_error("move_anim", 1),
            ERROR_PROFILE_APPLY_FILE_OPERATION_FAILED
        );
        assert_eq!(
            map_apply_step_to_error("move_anim", 13),
            ERROR_PROFILE_APPLY_PERMISSION_DENIED
        );
        assert_eq!(
            map_apply_step_to_error("ensure_dirs", 28),
            ERROR_REMOTE_DISK_FULL
        );
        assert_eq!(
            map_apply_step_to_error("restart", 1),
            ERROR_PROFILE_APPLY_RESTART_FAILED
        );
        assert_eq!(
            map_apply_step_to_error("restart_services", 210),
            ERROR_PROFILE_APPLY_FAILED
        );
    }
}
