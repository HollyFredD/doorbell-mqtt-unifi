//! profile.json, the doorbell .conf files (legacy apply) and IPC messages.

use crate::state::{Animation, ProfileState, Sound};
use crate::utils;

use serde_json::{Value, json};
use std::fs;
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Welcome {
    pub enabled: bool,
    pub file: String,
    pub count: i32,
    pub duration_ms: i32,
    pub loop_: bool,
    /// Optional, "WELCOME".
    pub gui_id: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RingButton {
    pub enabled: bool,
    pub file: String,
    pub repeat_times: i32,
    pub volume: i32,
    /// Optional, "RING_BUTTON_PRESSED".
    pub sound_state_name: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Profile {
    pub schema_version: i32,
    pub welcome: Welcome,
    pub ring_button: RingButton,
}

fn str_or_empty(obj: &Value, key: &str) -> String {
    utils::json_str(obj, key).unwrap_or("").to_string()
}

/// Loads `<dir>/profile.json`.
pub fn load_from_dir(dir: &Path) -> Option<Profile> {
    let path = utils::build_path(dir, "profile.json");

    if !utils::file_exists(&path) {
        log_error!("Profile.json does not exists in '{}'", path.display());
        return None;
    }

    let root = utils::read_json_file(&path)?;
    let mut p = Profile {
        schema_version: utils::json_int(&root, "schemaVersion").unwrap_or_else(|| {
            log_error!("Failed to load schema version using default of 1");
            1
        }),
        ..Profile::default()
    };

    if let Some(w) = root.get("welcome") {
        p.welcome = Welcome {
            enabled: utils::json_bool(w, "enabled").unwrap_or(false),
            file: str_or_empty(w, "file"),
            count: utils::json_int(w, "count").unwrap_or(0),
            duration_ms: utils::json_int(w, "durationMs").unwrap_or(0),
            loop_: utils::json_bool(w, "loop").unwrap_or(false),
            gui_id: str_or_empty(w, "guiId"),
        };
    }

    if let Some(r) = root.get("ringButton") {
        p.ring_button = RingButton {
            enabled: utils::json_bool(r, "enabled").unwrap_or(false),
            file: str_or_empty(r, "file"),
            repeat_times: utils::json_int(r, "repeatTimes").unwrap_or(0),
            volume: utils::json_int(r, "volume").unwrap_or(0),
            sound_state_name: str_or_empty(r, "soundStateName"),
        };
    }

    Some(p)
}

pub fn write_to_file(path: &Path, p: &Profile) -> bool {
    let root = json!({
        "schemaVersion": 1,
        "welcome": {
            "enabled": p.welcome.enabled,
            "file": p.welcome.file,
            "count": p.welcome.count,
            "durationMs": p.welcome.duration_ms,
            "loop": p.welcome.loop_,
            "guiId": p.welcome.gui_id,
        },
        "ringButton": {
            "enabled": p.ring_button.enabled,
            "file": p.ring_button.file,
            "repeatTimes": p.ring_button.repeat_times,
            "volume": p.ring_button.volume,
            "soundStateName": p.ring_button.sound_state_name,
        },
    });

    write_json(
        path,
        &serde_json::to_string_pretty(&root).unwrap_or_default(),
    )
}

fn write_json(path: &Path, text: &str) -> bool {
    fs::write(path, text)
        .map_err(|e| log_error!("Failed to write '{}': {}", path.display(), e))
        .is_ok()
}

fn read_conf(path: &Path) -> Option<Value> {
    let root = utils::read_json_file(path);
    if root.is_none() {
        log_error!("Failed to read config file: {}", path.display());
    }
    root
}

/// Fills `out.welcome` from the WELCOME entry of ubnt_lcm_gui.conf.
pub fn read_from_lcm_gui_conf(path: &Path, out: &mut Profile) -> bool {
    let Some(root) = read_conf(path) else {
        return false;
    };

    out.welcome = Welcome::default();

    let items = root
        .get("customAnimations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    if let Some(item) = items
        .filter(|i| i.is_object())
        .find(|i| utils::json_str(i, "guiId") == Some("WELCOME"))
    {
        out.welcome.gui_id = "WELCOME".to_string();
        out.welcome.file = str_or_empty(item, "file");
        out.welcome.count = utils::json_int(item, "count").unwrap_or(1);
        out.welcome.duration_ms = utils::json_int(item, "durationMs").unwrap_or(0);
        out.welcome.enabled = utils::json_bool(item, "enable").unwrap_or(false);
    }

    true
}

/// Fills `out.ring_button` from the RING_BUTTON_PRESSED entry of ubnt_sounds_leds.conf.
pub fn read_from_sounds_leds_conf(path: &Path, out: &mut Profile) -> bool {
    let Some(root) = read_conf(path) else {
        return false;
    };

    out.ring_button = RingButton {
        repeat_times: 1,
        volume: 100,
        ..RingButton::default()
    };

    let items = root
        .get("customSounds")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    if let Some(item) = items
        .filter(|i| i.is_object())
        .find(|i| utils::json_str(i, "soundStateName") == Some("RING_BUTTON_PRESSED"))
    {
        out.ring_button.sound_state_name = "RING_BUTTON_PRESSED".to_string();
        out.ring_button.enabled = utils::json_bool(item, "enable").unwrap_or(false);
        out.ring_button.file = str_or_empty(item, "file");
        out.ring_button.repeat_times = utils::json_int(item, "repeatTimes").unwrap_or(1);
        out.ring_button.volume = utils::json_int(item, "volume").unwrap_or(100);
    }

    true
}

/// Replaces `key` in the .conf at `in_path` (moved to the end, like the C service did) and writes `out_path`.
fn patch_conf(in_path: &Path, out_path: &Path, key: &str, value: Value) -> bool {
    let Some(mut root) = read_conf(in_path) else {
        return false;
    };

    let Some(obj) = root.as_object_mut() else {
        log_error!("'{}' is not a JSON object", in_path.display());
        return false;
    };

    obj.shift_remove(key);
    obj.insert(key.to_string(), value);

    write_json(out_path, &root.to_string())
}

pub fn patch_lcm_gui_conf(in_path: &Path, out_path: &Path, desired: &Profile) -> bool {
    patch_conf(
        in_path,
        out_path,
        "customAnimations",
        custom_animations_array(desired),
    )
}

pub fn patch_sounds_leds_conf(in_path: &Path, out_path: &Path, desired: &Profile) -> bool {
    if !desired.ring_button.enabled || desired.ring_button.file.is_empty() {
        log_debug!("Ring button disabled or no file set, copying config unchanged");

        return fs::rename(in_path, out_path)
            .map_err(|e| {
                log_error!(
                    "Failed to rename '{}' to '{}': {}",
                    in_path.display(),
                    out_path.display(),
                    e
                )
            })
            .is_ok();
    }

    patch_conf(
        in_path,
        out_path,
        "customSounds",
        custom_sounds_array(desired),
    )
}

pub fn custom_animations_array(p: &Profile) -> Value {
    json!([{
        "guiId": "WELCOME",
        "count": p.welcome.count,
        "durationMs": p.welcome.duration_ms,
        "enable": p.welcome.enabled,
        "file": p.welcome.file,
        "loop": p.welcome.loop_,
    }])
}

pub fn custom_sounds_array(p: &Profile) -> Value {
    json!([{
        "soundStateName": "RING_BUTTON_PRESSED",
        "enable": p.ring_button.enabled,
        "file": p.ring_button.file,
        "repeatTimes": p.ring_button.repeat_times,
        "volume": p.ring_button.volume,
    }])
}

pub fn write_ipc_lcm_gui_message(out_path: &Path, p: &Profile) -> bool {
    let msg = json!({ "functionName": "ChangeLcmGuiSettings", "customAnimations": custom_animations_array(p) });
    write_json(out_path, &msg.to_string())
}

pub fn write_ipc_sounds_leds_message(out_path: &Path, p: &Profile) -> bool {
    let msg =
        json!({ "functionName": "ChangeSoundLedSettings", "customSounds": custom_sounds_array(p) });
    write_json(out_path, &msg.to_string())
}

// ---- IPC responses ----

/// Raw stdout of the two ubnt_ipc_cli calls.
#[derive(Default)]
pub struct IpcRaw {
    pub lcm_gui_json: String,
    pub sounds_leds_json: String,
}

fn parse_animations(arr: Option<&Value>) -> Option<Vec<Animation>> {
    let items = arr?.as_array()?;
    Some(
        items
            .iter()
            .map(|item| Animation {
                count: utils::json_int(item, "count").unwrap_or(0),
                duration_ms: utils::json_int(item, "durationMs").unwrap_or(0),
                enable: utils::json_bool(item, "enable").unwrap_or(false),
                loop_: utils::json_bool(item, "loop").unwrap_or(false),
                file: utils::json_str(item, "file").map(str::to_string),
                gui_id: utils::json_str(item, "guiId").map(str::to_string),
            })
            .collect(),
    )
}

fn parse_sounds(arr: Option<&Value>) -> Option<Vec<Sound>> {
    let items = arr?.as_array()?;
    Some(
        items
            .iter()
            .map(|item| Sound {
                volume: utils::json_int(item, "volume").unwrap_or(0),
                repeat_times: utils::json_int(item, "repeatTimes").unwrap_or(0),
                enable: utils::json_bool(item, "enable").unwrap_or(false),
                file: utils::json_str(item, "file").map(str::to_string),
                sound_state_name: utils::json_str(item, "soundStateName").map(str::to_string),
            })
            .collect(),
    )
}

fn payload_item<'a>(root: &'a Value, key: &str) -> Option<&'a Value> {
    let payload = utils::get_ci(utils::get_ci(root, "response")?, "payload")?;
    utils::get_ci(payload, key)
}

/// Responses to the apply messages: `{"response":{"payload":{"customAnimations":[...]}}}`.
pub fn parse_apply_response(raw: &IpcRaw) -> Option<ProfileState> {
    let lcm = utils::parse_json_lenient(&raw.lcm_gui_json).ok()?;
    let custom_animations = parse_animations(payload_item(&lcm, "customAnimations"))?;

    let sound = utils::parse_json_lenient(&raw.sounds_leds_json).ok()?;
    let custom_sounds = parse_sounds(payload_item(&sound, "customSounds"))?;

    Some(ProfileState {
        custom_animations,
        custom_sounds,
    })
}

/// Responses to the config reads: bare arrays.
pub fn parse_validate_response(raw: &IpcRaw) -> Option<ProfileState> {
    let lcm = utils::parse_json_lenient(&raw.lcm_gui_json).ok()?;
    let sound = utils::parse_json_lenient(&raw.sounds_leds_json).ok()?;

    Some(ProfileState {
        custom_animations: parse_animations(Some(&lcm))?,
        custom_sounds: parse_sounds(Some(&sound))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_test_profile() {
        let p = load_from_dir(Path::new("test-profile")).unwrap();
        assert!(p.welcome.enabled);
        assert_eq!(p.welcome.file, "test.png");
        assert_eq!(p.welcome.count, 60);
        assert_eq!(p.ring_button.file, "test.ogg");
        assert_eq!(p.ring_button.volume, 100);
    }

    #[test]
    fn ipc_message_key_order() {
        let p = load_from_dir(Path::new("test-profile")).unwrap();
        let msg = json!({ "functionName": "ChangeLcmGuiSettings", "customAnimations": custom_animations_array(&p) });
        assert_eq!(
            msg.to_string(),
            r#"{"functionName":"ChangeLcmGuiSettings","customAnimations":[{"guiId":"WELCOME","count":60,"durationMs":1960,"enable":true,"file":"test.png","loop":false}]}"#
        );
    }

    #[test]
    fn parses_apply_and_validate_responses() {
        let raw = IpcRaw {
            lcm_gui_json: r#"{"response":{"payload":{"customAnimations":[{"guiId":"WELCOME","file":"a.png","count":3,"enable":true}]}}}"#.into(),
            sounds_leds_json: r#"{"response":{"payload":{"customSounds":[{"soundStateName":"RING_BUTTON_PRESSED","volume":80}]}}}"#.into(),
        };
        let state = parse_apply_response(&raw).unwrap();
        assert_eq!(state.custom_animations[0].count, 3);
        assert_eq!(state.custom_sounds[0].volume, 80);

        let raw = IpcRaw {
            lcm_gui_json: "[]".into(),
            sounds_leds_json: "[{\"volume\":5}]\n".into(),
        };
        let state = parse_validate_response(&raw).unwrap();
        assert!(state.custom_animations.is_empty());
        assert_eq!(state.custom_sounds[0].volume, 5);

        assert!(parse_validate_response(&IpcRaw::default()).is_none());
    }
}
