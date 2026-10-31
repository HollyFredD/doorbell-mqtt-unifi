//! Embedded web interface and REST API for configuring the doorbell.
//!
//! Runs an axum HTTP server on its own tokio runtime in a dedicated thread.
//! It reuses the exact same doorbell action code as the MQTT router and
//! publishes progress to Home Assistant, so the two control planes stay in sync.
//! A mutex serializes doorbell operations triggered from the web UI.

use crate::command::{self, Ctx};
use crate::config::ApplyMethod;
use crate::profile;
use crate::state;
use crate::utils;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Multipart, Path as AxPath, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The single-page interface, embedded at compile time.
pub const HTML: &str = include_str!("../web/index.html");

/// Directories owned by the service inside the profiles folder.
const SYSTEM_DIRS: [&str; 3] = ["tmp", "downloads", "partial"];

#[derive(Clone)]
struct WebState {
    ctx: Arc<Ctx>,
    config_path: PathBuf,
    profiles_dir: PathBuf,
    action_lock: Arc<Mutex<()>>,
}

fn json_ok(v: Value) -> Response {
    (Json(v)).into_response()
}

fn json_err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

fn is_system_dir(name: &str) -> bool {
    SYSTEM_DIRS.contains(&name)
}

fn list_files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.path().is_file() {
                if let Ok(name) = entry.file_name().into_string() {
                    out.push(name);
                }
            }
        }
    }
    out
}

fn profile_to_value(name: &str, p: &profile::Profile, files: Vec<String>) -> Value {
    let has_image = !p.welcome.file.is_empty() && files.iter().any(|f| *f == p.welcome.file);
    let has_sound =
        !p.ring_button.file.is_empty() && files.iter().any(|f| *f == p.ring_button.file);

    json!({
        "name": name,
        "files": files,
        "hasImage": has_image,
        "hasSound": has_sound,
        "welcome": {
            "enabled": p.welcome.enabled,
            "file": p.welcome.file,
            "count": p.welcome.count,
            "durationMs": p.welcome.duration_ms,
            "loop": p.welcome.loop_,
        },
        "ringButton": {
            "enabled": p.ring_button.enabled,
            "file": p.ring_button.file,
            "repeatTimes": p.ring_button.repeat_times,
            "volume": p.ring_button.volume,
        },
    })
}

fn list_profiles(root: &Path) -> Vec<Value> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if is_system_dir(&name) {
            continue;
        }
        if let Some(p) = profile::load_from_dir(&path) {
            let files = list_files(&path);
            out.push(profile_to_value(&name, &p, files));
        }
    }

    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    out
}

fn list_sound_files(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = list_files(dir)
        .into_iter()
        .filter(|f| {
            utils::is_valid_filename(f, utils::FileClass::Sound).is_ok()
        })
        .collect();
    out.sort();
    out
}

fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

fn default_profile() -> profile::Profile {
    profile::Profile {
        schema_version: 1,
        welcome: profile::Welcome {
            enabled: false,
            file: String::new(),
            count: 1,
            duration_ms: 0,
            loop_: false,
            gui_id: "WELCOME".to_string(),
        },
        ring_button: profile::RingButton {
            enabled: false,
            file: String::new(),
            repeat_times: 1,
            volume: 100,
            sound_state_name: "RING_BUTTON_PRESSED".to_string(),
        },
    }
}

/// Runs a doorbell action on a blocking thread, serialized by `action_lock`.
/// Returns success iff the action left no error on `Ha`.
async fn run_action(state: WebState, f: impl FnOnce(&Ctx) + Send + 'static) -> Response {
    let lock = state.action_lock.clone();
    let ctx = state.ctx.clone();

    let handle = tokio::task::spawn_blocking(move || {
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        f(&ctx);
        ctx.ha.last_error()
    });

    match handle.await {
        Ok((code, _)) if code == 0 => json_ok(json!({ "ok": true })),
        Ok((code, msg)) => {
            let message = if msg.is_empty() {
                "Action en échec".to_string()
            } else {
                msg
            };
            (
                StatusCode::CONFLICT,
                Json(json!({ "error": message, "code": code })),
            )
                .into_response()
        }
        Err(e) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Tâche interrompue : {e}"),
        ),
    }
}

// ---- Static ----

async fn index() -> Html<&'static str> {
    Html(HTML)
}

// ---- Bootstrap ----

async fn bootstrap(State(st): State<WebState>) -> Response {
    let ctx = &st.ctx;
    let raw_config = utils::read_json_file(&st.config_path);

    let last_applied = state::load(&ctx.state_dir).ok().map(|a| {
        json!({
            "profileName": a.profile_name,
            "appliedAt": a.applied_at,
            "isPreset": a.is_preset,
            "applyMethod": if a.apply_method == ApplyMethod::Ipc { "ipc" } else { "legacy" },
        })
    });

    let method = if ctx.cfg.apply_method == ApplyMethod::Ipc {
        "ipc"
    } else {
        "legacy"
    };

    json_ok(json!({
        "app": {
            "version": crate::APP_VERSION,
            "instance": ctx.cfg.mqtt.instance,
            "instanceHuman": ctx.cfg.mqtt.instance_human,
            "applyMethod": method,
        },
        "config": raw_config,
        "status": last_applied,
        "profiles": list_profiles(&st.profiles_dir),
        "presets": ctx.cfg.presets.iter().map(|p| json!({
            "name": p.display_name,
            "directory": p.directory,
        })).collect::<Vec<_>>(),
        "sfx": {
            "defaultVolume": ctx.cfg.sfx_default_volume,
            "presets": ctx.cfg.sfx_presets.iter().map(|p| json!({
                "name": p.name,
                "file": p.file,
                "volume": p.volume,
            })).collect::<Vec<_>>(),
        },
        "sfxFiles": list_sound_files(&ctx.sounds_dir),
        "paths": {
            "configPath": st.config_path.display().to_string(),
            "profilesDir": st.profiles_dir.display().to_string(),
            "soundsDir": ctx.sounds_dir.display().to_string(),
        },
    }))
}

// ---- Config ----

fn validate_config_value(v: &Value) -> Option<String> {
    let obj = match v.as_object() {
        Some(o) => o,
        None => return Some("La configuration doit être un objet JSON".into()),
    };

    let mqtt = match obj.get("mqtt") {
        Some(Value::Object(m)) => m,
        _ => return Some("Section « mqtt » manquante ou invalide".into()),
    };
    if !mqtt.get("host").is_some_and(Value::is_string) {
        return Some("« mqtt.host » doit être une chaîne de caractères".into());
    }

    let ssh = match obj.get("ssh") {
        Some(Value::Object(s)) => s,
        _ => return Some("Section « ssh » manquante ou invalide".into()),
    };
    if !ssh.get("host").is_some_and(Value::is_string) {
        return Some("« ssh.host » doit être une chaîne de caractères".into());
    }

    if let Some(p) = obj.get("presets") {
        let arr = match p.as_array() {
            Some(a) => a,
            None => return Some("« presets » doit être une liste".into()),
        };
        for (i, item) in arr.iter().enumerate() {
            let it = match item.as_object() {
                Some(o) => o,
                None => return Some(format!("Preset {} : objet invalide", i + 1)),
            };
            if !it.get("name").is_some_and(Value::is_string) {
                return Some(format!("Preset {} : « name » requis", i + 1));
            }
            if !it.get("directory").is_some_and(Value::is_string) {
                return Some(format!("Preset {} : « directory » requis", i + 1));
            }
        }
    }

    if let Some(s) = obj.get("sfx") {
        let s = match s.as_object() {
            Some(o) => o,
            None => return Some("« sfx » doit être un objet".into()),
        };
        if let Some(dv) = s.get("defaultVolume") {
            if !dv.is_number() {
                return Some("« sfx.defaultVolume » doit être un nombre".into());
            }
        }
        if let Some(p) = s.get("presets") {
            let arr = match p.as_array() {
                Some(a) => a,
                None => return Some("« sfx.presets » doit être une liste".into()),
            };
            for (i, item) in arr.iter().enumerate() {
                let it = match item.as_object() {
                    Some(o) => o,
                    None => return Some(format!("SFX {} : objet invalide", i + 1)),
                };
                if !it.get("name").is_some_and(Value::is_string) {
                    return Some(format!("SFX {} : « name » requis", i + 1));
                }
                if !it.get("file").is_some_and(Value::is_string) {
                    return Some(format!("SFX {} : « file » requis", i + 1));
                }
            }
        }
    }

    if let Some(u) = obj.get("unifi") {
        let u = match u.as_object() {
            Some(o) => o,
            None => return Some("« unifi » doit être un objet".into()),
        };
        if let Some(m) = u.get("apply_method") {
            let m = match m.as_str() {
                Some(x) => x,
                None => return Some("« unifi.apply_method » doit être une chaîne".into()),
            };
            if !m.eq_ignore_ascii_case("ipc") && !m.eq_ignore_ascii_case("legacy") {
                return Some(format!("« unifi.apply_method » invalide : {m}"));
            }
        }
    }

    None
}

async fn update_config(State(st): State<WebState>, Json(body): Json<Value>) -> Response {
    if let Some(msg) = validate_config_value(&body) {
        return json_err(StatusCode::BAD_REQUEST, msg);
    }

    let text = serde_json::to_string_pretty(&body).unwrap_or_default();
    match std::fs::write(&st.config_path, text) {
        Ok(()) => json_ok(json!({ "ok": true })),
        Err(e) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Écriture impossible : {e}"),
        ),
    }
}

// ---- Profiles ----

async fn create_profile(State(st): State<WebState>, Json(body): Json<Value>) -> Response {
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if name.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "Le nom du profil est requis");
    }

    let dir = slugify(name);
    if dir.is_empty() {
        return json_err(
            StatusCode::BAD_REQUEST,
            "Le nom doit contenir au moins une lettre ou un chiffre",
        );
    }
    if !utils::is_valid_directory_name(&dir) {
        return json_err(StatusCode::BAD_REQUEST, "Nom de dossier invalide");
    }
    if is_system_dir(&dir) {
        return json_err(StatusCode::BAD_REQUEST, "Ce nom est réservé");
    }

    let path = utils::build_path(&st.profiles_dir, &dir);
    if path.exists() {
        return json_err(StatusCode::CONFLICT, "Un profil avec ce nom existe déjà");
    }

    if !utils::create_directory(&path) {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, "Création du dossier impossible");
    }

    if !profile::write_to_file(&utils::build_path(&path, "profile.json"), &default_profile()) {
        return json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Écriture du profil impossible",
        );
    }

    let p = profile::load_from_dir(&path).unwrap_or_else(default_profile);
    json_ok(profile_to_value(&dir, &p, list_files(&path)))
}

async fn delete_profile(State(st): State<WebState>, AxPath(name): AxPath<String>) -> Response {
    if !utils::is_valid_directory_name(&name) || is_system_dir(&name) {
        return json_err(StatusCode::BAD_REQUEST, "Nom de profil invalide");
    }

    let path = utils::build_path(&st.profiles_dir, &name);
    if !path.is_dir() {
        return json_err(StatusCode::NOT_FOUND, "Profil introuvable");
    }

    if let Some(p) = st
        .ctx
        .cfg
        .presets
        .iter()
        .find(|p| p.directory == name)
    {
        return json_err(
            StatusCode::CONFLICT,
            format!(
                "Référencé par le preset « {} » — retirez-le dans la section Configuration d'abord",
                p.display_name
            ),
        );
    }

    match std::fs::remove_dir_all(&path) {
        Ok(()) => json_ok(json!({ "ok": true })),
        Err(e) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Suppression impossible : {e}"),
        ),
    }
}

async fn update_metadata(
    State(st): State<WebState>,
    AxPath(name): AxPath<String>,
    Json(body): Json<Value>,
) -> Response {
    if !utils::is_valid_directory_name(&name) || is_system_dir(&name) {
        return json_err(StatusCode::BAD_REQUEST, "Nom de profil invalide");
    }
    let dir = utils::build_path(&st.profiles_dir, &name);
    if !dir.is_dir() {
        return json_err(StatusCode::NOT_FOUND, "Profil introuvable");
    }

    let mut p = match profile::load_from_dir(&dir) {
        Some(p) => p,
        None => return json_err(StatusCode::NOT_FOUND, "profile.json introuvable"),
    };

    let apply_fields = |target: &mut String, value: Option<&Value>| {
        if let Some(v) = value.and_then(Value::as_str) {
            *target = v.to_string();
        }
    };

    if let Some(w) = body.get("welcome").and_then(Value::as_object) {
        if let Some(v) = w.get("enabled") {
            p.welcome.enabled = v.as_bool().unwrap_or(p.welcome.enabled);
        }
        if let Some(v) = w.get("loop") {
            p.welcome.loop_ = v.as_bool().unwrap_or(p.welcome.loop_);
        }
        if let Some(v) = w.get("count") {
            p.welcome.count = v.as_f64().map(|f| f as i32).unwrap_or(p.welcome.count);
        }
        if let Some(v) = w.get("durationMs") {
            p.welcome.duration_ms = v.as_f64().map(|f| f as i32).unwrap_or(p.welcome.duration_ms);
        }
        apply_fields(&mut p.welcome.file, w.get("file"));
    }

    if let Some(r) = body.get("ringButton").and_then(Value::as_object) {
        if let Some(v) = r.get("enabled") {
            p.ring_button.enabled = v.as_bool().unwrap_or(p.ring_button.enabled);
        }
        if let Some(v) = r.get("repeatTimes") {
            p.ring_button.repeat_times = v.as_f64().map(|f| f as i32).unwrap_or(p.ring_button.repeat_times);
        }
        if let Some(v) = r.get("volume") {
            p.ring_button.volume = v.as_f64().map(|f| f as i32).unwrap_or(p.ring_button.volume);
        }
        apply_fields(&mut p.ring_button.file, r.get("file"));
    }

    if !profile::write_to_file(&utils::build_path(&dir, "profile.json"), &p) {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, "Écriture du profil impossible");
    }

    json_ok(profile_to_value(&name, &p, list_files(&dir)))
}

/// Saves an uploaded asset into a profile, validates it and updates profile.json.
async fn upload_asset(
    State(st): State<WebState>,
    AxPath(name): AxPath<String>,
    mut mp: Multipart,
) -> Response {
    if !utils::is_valid_directory_name(&name) || is_system_dir(&name) {
        return json_err(StatusCode::BAD_REQUEST, "Nom de profil invalide");
    }
    let dir = utils::build_path(&st.profiles_dir, &name);
    if !dir.is_dir() {
        return json_err(StatusCode::NOT_FOUND, "Profil introuvable");
    }

    let mut kind: Option<String> = None;
    let mut filename: Option<String> = None;
    let mut bytes: Option<Vec<u8>> = None;

    loop {
        let field = match mp.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(_) => return json_err(StatusCode::BAD_REQUEST, "Upload mal formé"),
        };
        match field.name() {
            Some("kind") => kind = field.text().await.ok().map(|s| s.trim().to_string()),
            Some("file") => {
                filename = field.file_name().map(String::from);
                match field.bytes().await {
                    Ok(b) => bytes = Some(b.to_vec()),
                    Err(_) => return json_err(StatusCode::PAYLOAD_TOO_LARGE, "Fichier trop volumineux"),
                }
            }
            _ => {}
        }
    }

    let kind = kind.unwrap_or_default();
    let fname = filename.unwrap_or_default();
    let data = match bytes {
        Some(b) if !b.is_empty() => b,
        _ => return json_err(StatusCode::BAD_REQUEST, "Fichier manquant"),
    };

    let (cls, section) = match kind.as_str() {
        "image" => (utils::FileClass::Animation, "welcome"),
        "sound" => (utils::FileClass::Sound, "ringButton"),
        _ => return json_err(StatusCode::BAD_REQUEST, "Champ « kind » invalide (image|sound)"),
    };

    if fname.eq_ignore_ascii_case("profile.json") {
        return json_err(StatusCode::BAD_REQUEST, "Ce nom de fichier est réservé");
    }
    if let Err(e) = utils::is_valid_filename(&fname, cls) {
        return json_err(StatusCode::BAD_REQUEST, e.message);
    }

    let target = utils::build_path(&dir, &fname);
    let suffix = format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let tmp = utils::build_path(&dir, &format!(".{suffix}.{fname}"));

    if std::fs::write(&tmp, &data).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, "Écriture du fichier impossible");
    }

    if let Err(e) = utils::is_valid_file(&tmp, cls) {
        let _ = std::fs::remove_file(&tmp);
        return json_err(StatusCode::BAD_REQUEST, e.message);
    }

    if std::fs::rename(&tmp, &target).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, "Déplacement du fichier impossible");
    }

    let mut p = profile::load_from_dir(&dir).unwrap_or_else(default_profile);
    if section == "welcome" {
        p.welcome.file = fname.clone();
        p.welcome.enabled = true;
    } else {
        p.ring_button.file = fname.clone();
        p.ring_button.enabled = true;
    }

    if !profile::write_to_file(&utils::build_path(&dir, "profile.json"), &p) {
        let _ = std::fs::remove_file(&target);
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, "Mise à jour du profil impossible");
    }

    json_ok(profile_to_value(&name, &p, list_files(&dir)))
}

fn resolve_profile(state: &WebState, name: &str) -> Option<(PathBuf, profile::Profile)> {
    if !utils::is_valid_directory_name(name) || is_system_dir(name) {
        return None;
    }
    let dir = utils::build_path(&state.profiles_dir, name);
    if !dir.is_dir() {
        return None;
    }
    let p = profile::load_from_dir(&dir)?;
    Some((dir, p))
}

fn serve_file(path: &Path, mime: &str) -> Response {
    match std::fs::read(path) {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime)
            .header(header::CACHE_CONTROL, "private, max-age=3600")
            .body(Body::from(bytes))
            .unwrap(),
        Err(_) => json_err(StatusCode::NOT_FOUND, "Fichier introuvable"),
    }
}

async fn profile_image(
    State(st): State<WebState>,
    AxPath(name): AxPath<String>,
) -> Response {
    let Some((dir, p)) = resolve_profile(&st, &name) else {
        return json_err(StatusCode::NOT_FOUND, "Profil introuvable");
    };
    if p.welcome.file.is_empty() {
        return json_err(StatusCode::NOT_FOUND, "Aucune image configurée");
    }
    serve_file(&utils::build_path(&dir, &p.welcome.file), "image/png")
}

async fn profile_sound(
    State(st): State<WebState>,
    AxPath(name): AxPath<String>,
) -> Response {
    let Some((dir, p)) = resolve_profile(&st, &name) else {
        return json_err(StatusCode::NOT_FOUND, "Profil introuvable");
    };
    if p.ring_button.file.is_empty() {
        return json_err(StatusCode::NOT_FOUND, "Aucune sonnerie configurée");
    }
    let mime = if p.ring_button.file.to_ascii_lowercase().ends_with(".wav") {
        "audio/wav"
    } else {
        "audio/ogg"
    };
    serve_file(&utils::build_path(&dir, &p.ring_button.file), mime)
}

async fn apply_profile(State(st): State<WebState>, AxPath(name): AxPath<String>) -> Response {
    if !utils::is_valid_directory_name(&name) || is_system_dir(&name) {
        return json_err(StatusCode::BAD_REQUEST, "Nom de profil invalide");
    }
    let dir = utils::build_path(&st.profiles_dir, &name);
    if !dir.is_dir() {
        return json_err(StatusCode::NOT_FOUND, "Profil introuvable");
    }

    run_action(st, move |ctx| {
        command::apply_profile_dir(ctx, &dir, &name);
    }).await
}

// ---- Sounds ----

async fn upload_sound(State(st): State<WebState>, mut mp: Multipart) -> Response {
    let mut filename: Option<String> = None;
    let mut bytes: Option<Vec<u8>> = None;

    while let Some(field) = mp
        .next_field()
        .await
        .map_err(|_| json_err(StatusCode::BAD_REQUEST, "Upload mal formé"))
        .ok()
        .flatten()
    {
        match field.name() {
            Some("file") => {
                filename = field.file_name().map(String::from);
                match field.bytes().await {
                    Ok(b) => bytes = Some(b.to_vec()),
                    Err(_) => return json_err(StatusCode::PAYLOAD_TOO_LARGE, "Fichier trop volumineux"),
                }
            }
            _ => {}
        }
    }

    let fname = filename.unwrap_or_default();
    let data = match bytes {
        Some(b) if !b.is_empty() => b,
        _ => return json_err(StatusCode::BAD_REQUEST, "Fichier manquant"),
    };

    if let Err(e) = utils::is_valid_filename(&fname, utils::FileClass::Sound) {
        return json_err(StatusCode::BAD_REQUEST, e.message);
    }

    let target = utils::build_path(&st.ctx.sounds_dir, &fname);
    let suffix = format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let tmp = utils::build_path(&st.ctx.sounds_dir, &format!(".{suffix}.{fname}"));

    if !utils::directory_exists(&st.ctx.sounds_dir) {
        utils::create_directory(&st.ctx.sounds_dir);
    }

    if std::fs::write(&tmp, &data).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, "Écriture du fichier impossible");
    }

    if let Err(e) = utils::is_valid_file(&tmp, utils::FileClass::Sound) {
        let _ = std::fs::remove_file(&tmp);
        return json_err(StatusCode::BAD_REQUEST, e.message);
    }

    if std::fs::rename(&tmp, &target).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, "Déplacement du fichier impossible");
    }

    json_ok(json!({ "ok": true, "file": fname }))
}

/// Streams a sound file from the sounds directory so the browser can preview it.
async fn stream_sound(State(st): State<WebState>, AxPath(file): AxPath<String>) -> Response {
    if utils::is_valid_filename(&file, utils::FileClass::Sound).is_err() {
        return json_err(StatusCode::BAD_REQUEST, "Nom de fichier invalide");
    }
    let mime = if file.to_ascii_lowercase().ends_with(".wav") {
        "audio/wav"
    } else {
        "audio/ogg"
    };
    serve_file(&utils::build_path(&st.ctx.sounds_dir, &file), mime)
}

async fn delete_sound(State(st): State<WebState>, AxPath(file): AxPath<String>) -> Response {
    if utils::is_valid_filename(&file, utils::FileClass::Sound).is_err() {
        return json_err(StatusCode::BAD_REQUEST, "Nom de fichier invalide");
    }

    if let Some(s) = st
        .ctx
        .cfg
        .sfx_presets
        .iter()
        .find(|s| s.file.eq_ignore_ascii_case(&file))
    {
        return json_err(
            StatusCode::CONFLICT,
            format!(
                "Référencé par le SFX « {} » — retirez-le dans la section Configuration d'abord",
                s.name
            ),
        );
    }

    let path = utils::build_path(&st.ctx.sounds_dir, &file);
    if !path.is_file() {
        return json_err(StatusCode::NOT_FOUND, "Fichier introuvable");
    }

    match std::fs::remove_file(&path) {
        Ok(()) => json_ok(json!({ "ok": true })),
        Err(e) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Suppression impossible : {e}"),
        ),
    }
}

// ---- Actions ----

async fn action_preset(State(st): State<WebState>, Json(body): Json<Value>) -> Response {
    let name = body.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    run_action(st, move |ctx| {
        command::set_preset(ctx, &name);
    }).await
}

async fn action_test(State(st): State<WebState>) -> Response {
    run_action(st, |ctx| {
        command::test_config(ctx, "test-config");
    }).await
}

async fn action_validate(State(st): State<WebState>) -> Response {
    run_action(st, |ctx| {
        command::validate_profile(ctx, "");
    }).await
}

async fn action_reapply(State(st): State<WebState>) -> Response {
    run_action(st, |ctx| {
        command::reapply_last_profile(ctx, "");
    }).await
}

async fn action_download(State(st): State<WebState>) -> Response {
    run_action(st, |ctx| {
        command::download_assets(ctx, "download_assets");
    }).await
}

async fn action_sfx(State(st): State<WebState>, Json(body): Json<Value>) -> Response {
    let file = body.get("file").and_then(Value::as_str).unwrap_or("").to_string();
    if file.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "Fichier requis");
    }
    let volume = body
        .get("volume")
        .and_then(Value::as_i64)
        .unwrap_or(st.ctx.cfg.sfx_default_volume as i64)
        .clamp(0, 100) as i32;

    run_action(st, move |ctx| {
        command::play_sound_file(ctx, &file, volume);
    }).await
}

// ---- Server ----

/// Builds the router and blocks on the tokio runtime (intended for a thread).
pub fn run(ctx: Arc<Ctx>, config_path: PathBuf, profiles_dir: PathBuf, bind: &str, port: u16) {
    let state = WebState {
        ctx,
        config_path,
        profiles_dir,
        action_lock: Arc::new(Mutex::new(())),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/api/bootstrap", get(bootstrap))
        .route("/api/config", put(update_config))
        .route("/api/profiles", post(create_profile))
        .route("/api/profiles/{name}", delete(delete_profile))
        .route("/api/profiles/{name}/metadata", put(update_metadata))
        .route("/api/profiles/{name}/upload", post(upload_asset))
        .route("/api/profiles/{name}/image", get(profile_image))
        .route("/api/profiles/{name}/sound", get(profile_sound))
        .route("/api/profiles/{name}/apply", post(apply_profile))
        .route("/api/sounds", post(upload_sound))
        .route("/api/sounds/{file}", get(stream_sound).delete(delete_sound))
        .route("/api/actions/preset", post(action_preset))
        .route("/api/actions/test", post(action_test))
        .route("/api/actions/validate", post(action_validate))
        .route("/api/actions/reapply", post(action_reapply))
        .route("/api/actions/download", post(action_download))
        .route("/api/actions/sfx", post(action_sfx))
        .with_state(state)
        .layer(DefaultBodyLimit::max(4 * 1024 * 1024));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("web-ui")
        .enable_all()
        .build()
        .expect("tokio runtime for web UI");

    let addr = format!("{bind}:{port}");
    rt.block_on(async move {
        match tokio::net::TcpListener::bind(&addr).await {
            Ok(listener) => {
                let local = listener
                    .local_addr()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|_| addr.clone());
                log_info!("Web interface listening on http://{local}");
                if let Err(e) = axum::serve(listener, app).await {
                    log_error!("Web server error: {e}");
                }
            }
            Err(e) => {
                log_error!("Failed to bind web interface on {addr}: {e}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_good_config() {
        let cfg = json!({
            "mqtt": { "host": "192.168.1.5", "port": 1883 },
            "ssh": { "host": "10.0.0.1" },
            "unifi": { "apply_method": "ipc" },
            "presets": [{ "name": "Christmas", "directory": "christmas" }],
            "sfx": { "defaultVolume": 75, "presets": [{ "name": "X", "file": "x.ogg" }] },
        });
        assert_eq!(validate_config_value(&cfg), None);
    }

    #[test]
    fn rejects_missing_required_sections() {
        assert!(validate_config_value(&json!({ "mqtt": { "host": "h" } })).is_some());
        assert!(validate_config_value(&json!({
            "mqtt": { "host": "h" }, "ssh": {}
        })).is_some());
        assert!(validate_config_value(&json!({})).is_some());
    }

    #[test]
    fn rejects_bad_apply_method_and_presets() {
        let bad_method = json!({
            "mqtt": { "host": "h" }, "ssh": { "host": "d" },
            "unifi": { "apply_method": "nope" }
        });
        let m = validate_config_value(&bad_method).unwrap();
        assert!(m.contains("apply_method"));

        let bad_preset = json!({
            "mqtt": { "host": "h" }, "ssh": { "host": "d" },
            "presets": [{ "name": "Christmas" }]
        });
        assert!(validate_config_value(&bad_preset).unwrap().contains("directory"));
    }

    #[test]
    fn slugify_rules() {
        assert_eq!(slugify("Vacances 2024 !"), "vacances_2024");
        assert_eq!(slugify("Halloween"), "halloween");
        assert_eq!(slugify(" Christmas "), "christmas");
        assert_eq!(slugify("---"), "");
        assert_eq!(slugify("Téléviseur"), "t_l_viseur");
    }
}