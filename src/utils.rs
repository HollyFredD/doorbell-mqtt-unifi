use crate::errors::*;

use chrono::{DateTime, Utc};
use md5::{Digest, Md5};
use serde_json::Value;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_ASSET_SIZE_BYTES: u64 = 1_048_576;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum FileClass {
    Sound,
    Animation,
}

#[derive(Clone, Copy, PartialEq)]
enum DetectedType {
    Ogg,
    Wav,
    Png,
}

fn file_type_to_string(t: DetectedType) -> &'static str {
    match t {
        DetectedType::Ogg => "ogg",
        DetectedType::Wav => "wav",
        DetectedType::Png => "png",
    }
}

fn file_class_to_string(cls: FileClass) -> &'static str {
    match cls {
        FileClass::Animation => "animation (png)",
        FileClass::Sound => "sound (ogg/wav)",
    }
}

fn has_ext_ci(name: &str, ext: &str) -> bool {
    name.rfind('.')
        .is_some_and(|dot| name[dot..].eq_ignore_ascii_case(ext))
}

fn is_safe_single_segment(name: &str) -> bool {
    if name.is_empty() || name.len() > 255 {
        return false;
    }

    // Reject hidden files like ".env", path traversal, separators,
    // non-printables and a trailing space/dot.
    !name.starts_with('.')
        && !name.contains("..")
        && !name.contains('/')
        && !name.contains('\\')
        && name.bytes().all(|b| (0x20..=0x7e).contains(&b))
        && !name.ends_with(' ')
        && !name.ends_with('.')
}

fn ext_allowed_for_class(name: &str, cls: FileClass) -> bool {
    match cls {
        FileClass::Sound => has_ext_ci(name, ".ogg") || has_ext_ci(name, ".wav"),
        FileClass::Animation => has_ext_ci(name, ".png"),
    }
}

fn detect_type_from_ext(path: &str) -> Option<DetectedType> {
    if has_ext_ci(path, ".ogg") {
        Some(DetectedType::Ogg)
    } else if has_ext_ci(path, ".wav") {
        Some(DetectedType::Wav)
    } else if has_ext_ci(path, ".png") {
        Some(DetectedType::Png)
    } else {
        None
    }
}

fn detected_type_allowed_for_class(t: DetectedType, cls: FileClass) -> bool {
    match cls {
        FileClass::Sound => t == DetectedType::Ogg || t == DetectedType::Wav,
        FileClass::Animation => t == DetectedType::Png,
    }
}

fn has_valid_magic(f: &mut File, t: DetectedType) -> bool {
    let mut h = [0u8; 12];
    let need = match t {
        DetectedType::Ogg => 4,
        DetectedType::Wav => 12,
        DetectedType::Png => 8,
    };

    if f.read_exact(&mut h[..need]).is_err() {
        return false;
    }

    match t {
        DetectedType::Ogg => &h[..4] == b"OggS",
        DetectedType::Wav => &h[..4] == b"RIFF" && &h[8..12] == b"WAVE",
        DetectedType::Png => h[..8] == [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
    }
}

pub fn file_exists(path: &Path) -> bool {
    path.is_file()
}

pub fn directory_exists(path: &Path) -> bool {
    path.is_dir()
}

/// "base/child" with trailing slashes stripped from base. Plain concatenation on purpose:
/// unlike `Path::join`, an absolute child never replaces the base.
pub fn build_path(base: impl AsRef<Path>, child: &str) -> PathBuf {
    let base = base.as_ref().to_string_lossy();
    PathBuf::from(format!("{}/{}", base.trim_end_matches('/'), child))
}

/// Single segment, no path traversal, reasonable length.
pub fn is_valid_directory_name(name: &str) -> bool {
    is_safe_single_segment(name)
}

pub fn is_valid_filename(name: &str, cls: FileClass) -> AppResult {
    if !is_safe_single_segment(name) {
        return Err(AppError::new(ERROR_FILENAME_INVALID, "Filename is unsafe"));
    }

    if !ext_allowed_for_class(name, cls) {
        return Err(AppError::new(
            ERROR_FILE_EXTENSION_INVALID,
            "Invalid file extension",
        ));
    }

    Ok(())
}

/// Must exist, be a non-empty regular file under 1 MiB, with an extension and
/// magic header matching the class.
pub fn is_valid_file(path: &Path, cls: FileClass) -> AppResult {
    let display = path.display();

    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) => {
            log_warn!("stat failed for '{}': {}", display, e);
            return Err(AppError::new(
                ERROR_FILE_TYPE_INVALID,
                format!("Invalid file {e}"),
            ));
        }
    };

    if !meta.is_file() {
        log_error!("Path '{}' exists but is not a regular file", display);
        return Err(AppError::new(
            ERROR_FILE_TYPE_INVALID,
            "Invalid file: not a regular file",
        ));
    }

    if meta.len() == 0 {
        log_error!("'{}' is an empty file", display);
        return Err(AppError::new(ERROR_FILE_TYPE_INVALID, "File is empty"));
    }

    if meta.len() > MAX_ASSET_SIZE_BYTES {
        log_error!(
            "File '{}' exceeds max allowed size ({} bytes). Actual size={} bytes.",
            display,
            MAX_ASSET_SIZE_BYTES,
            meta.len()
        );
        return Err(AppError::new(
            ERROR_FILE_TYPE_INVALID,
            format!("File exceeds max size ({MAX_ASSET_SIZE_BYTES} bytes)"),
        ));
    }

    let Some(t) = detect_type_from_ext(&path.to_string_lossy()) else {
        log_error!("File '{}' does not have a valid asset extension", display);
        return Err(AppError::new(
            ERROR_FILE_TYPE_INVALID,
            "File does not have a valid extension",
        ));
    };

    if !detected_type_allowed_for_class(t, cls) {
        log_error!(
            "File '{}' is not a valid {} file (detected type={})",
            display,
            file_type_to_string(t),
            file_class_to_string(cls)
        );
        return Err(AppError::new(
            ERROR_FILE_TYPE_INVALID,
            format!(
                "File is not a valid {} file (detected type={})",
                file_type_to_string(t),
                file_class_to_string(cls)
            ),
        ));
    }

    let mut f = File::open(path).map_err(|e| {
        log_error!("Failed to open file '{}' : {}", display, e);
        AppError::new(ERROR_FILE_TYPE_INVALID, format!("Failed to open file {e}"))
    })?;

    if !has_valid_magic(&mut f, t) {
        log_error!(
            "File '{}' does not contain a valid {} header.",
            display,
            file_type_to_string(t)
        );
        return Err(AppError::new(
            ERROR_FILE_TYPE_INVALID,
            format!(
                "File does not contain a valid {} header",
                file_type_to_string(t)
            ),
        ));
    }

    Ok(())
}

pub fn md5_file_hex(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Md5::new();
    io::copy(&mut file, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// "front_door" -> "Front Door".
pub fn to_human_readable(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut capitalize_next = true;

    for c in input.chars() {
        if c == '_' || c == '-' {
            out.push(' ');
            capitalize_next = true;
        } else if capitalize_next {
            out.push(c.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            out.push(c);
        }
    }

    out
}

/// "YYYYMMDD_HHMMSS" (UTC).
pub fn timestamp_dir(t: &DateTime<Utc>) -> String {
    t.format("%Y%m%d_%H%M%S").to_string()
}

/// ISO 8601, e.g. "2024-06-01T12:34:56Z".
pub fn iso_timestamp(t: &DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// mkdir, logging like the service always has; an existing directory is fine.
pub fn create_directory(path: &Path) -> bool {
    match fs::create_dir(path) {
        Ok(()) => {
            log_info!("Directory '{}' created successfully.", path.display());
            true
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            log_info!("Directory '{}' already exists.", path.display());
            true
        }
        Err(e) => {
            log_error!("Failed to create directory '{}': {}", path.display(), e);
            false
        }
    }
}

/// mkdtemp(3): `prefix` followed by six random characters, created with mode 0700.
pub fn make_temp_dir(prefix: &str) -> io::Result<PathBuf> {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

    let mut seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
        ^ (u64::from(std::process::id()) << 32);

    for _ in 0..100 {
        let mut suffix = String::with_capacity(6);
        for _ in 0..6 {
            // xorshift64: good enough for a name, uniqueness comes from create() failing on collision
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            suffix.push(CHARS[(seed % CHARS.len() as u64) as usize] as char);
        }

        let path = PathBuf::from(format!("{prefix}{suffix}"));
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no unique temp directory name found",
    ))
}

/// rename(2), falling back to copy + unlink across filesystems.
pub fn move_file_cross_fs(old_path: &Path, new_path: &Path) -> io::Result<()> {
    if fs::rename(old_path, new_path).is_ok() {
        return Ok(());
    }

    fs::copy(old_path, new_path)?;
    fs::remove_file(old_path)
}

// ---- JSON helpers (serde_json::Value) ----

/// Parses the first JSON value and ignores anything after it (like cJSON with
/// require_null_terminated = false). Doorbell .conf files may end with NUL bytes.
pub fn parse_json_lenient(text: &str) -> Result<Value, serde_json::Error> {
    serde_json::Deserializer::from_str(text)
        .into_iter::<Value>()
        .next()
        .unwrap_or_else(|| serde_json::from_str::<Value>(""))
}

pub fn read_json_file(path: &Path) -> Option<Value> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            log_error!(
                "File '{}' does not exist or is not accessible: {}",
                path.display(),
                e
            );
            return None;
        }
    };

    match parse_json_lenient(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            log_error!("JSON parsing error in {}: {}", path.display(), e);
            None
        }
    }
}

/// Case-insensitive object lookup (cJSON_GetObjectItem).
pub fn get_ci<'a>(obj: &'a Value, key: &str) -> Option<&'a Value> {
    obj.as_object()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v)
}

pub fn json_str<'a>(obj: &'a Value, key: &str) -> Option<&'a str> {
    obj.get(key)?.as_str()
}

/// Any number, truncated to i32 like cJSON's valueint.
pub fn json_int(obj: &Value, key: &str) -> Option<i32> {
    obj.get(key)?.as_f64().map(|f| f as i32)
}

pub fn json_f64(obj: &Value, key: &str) -> Option<f64> {
    obj.get(key)?.as_f64()
}

pub fn json_bool(obj: &Value, key: &str) -> Option<bool> {
    obj.get(key)?.as_bool()
}

/// Trimmed and lowercased string value; None when missing, not a string or blank.
pub fn json_str_normalized(obj: &Value, key: &str) -> Option<String> {
    let is_space = |c: char| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r');
    let s = json_str(obj, key)?.trim_matches(is_space);
    (!s.is_empty()).then(|| s.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn valid_filename_image_file() {
        assert!(is_valid_filename("test.png", FileClass::Animation).is_ok());
    }

    #[test]
    fn valid_filename_sound_file() {
        assert!(is_valid_filename("test.wav", FileClass::Sound).is_ok());
        assert!(is_valid_filename("test.ogg", FileClass::Sound).is_ok());
    }

    #[test]
    fn invalid_filename_extensions() {
        assert_eq!(
            is_valid_filename("test.pdf", FileClass::Animation)
                .unwrap_err()
                .code,
            ERROR_FILE_EXTENSION_INVALID
        );
        assert_eq!(
            is_valid_filename("test.pdf", FileClass::Sound)
                .unwrap_err()
                .code,
            ERROR_FILE_EXTENSION_INVALID
        );
    }

    #[test]
    fn unsafe_filenames_rejected() {
        for name in [
            "../x.png",
            ".hidden.png",
            "a/b.png",
            "x.png.",
            "caf\u{e9}.png",
            "",
        ] {
            assert_eq!(
                is_valid_filename(name, FileClass::Animation)
                    .unwrap_err()
                    .code,
                ERROR_FILENAME_INVALID,
                "{name}"
            );
        }
    }

    #[test]
    fn valid_files() {
        assert!(is_valid_file(Path::new("test-profile/test.png"), FileClass::Animation).is_ok());
        assert!(is_valid_file(Path::new("test-profile/test.ogg"), FileClass::Sound).is_ok());
    }

    #[test]
    fn gif_with_png_extension_rejected() {
        let err = is_valid_file(
            Path::new("tests/fixtures/invalid_assets/gif_with_png_extension.png"),
            FileClass::Animation,
        );
        assert_eq!(err.unwrap_err().code, ERROR_FILE_TYPE_INVALID);
    }

    #[test]
    fn json_str_missing_or_wrong_type_is_none() {
        let obj = json!({"file": "a.png", "count": 1});
        assert_eq!(json_str(&obj, "file"), Some("a.png"));
        assert_eq!(json_str(&obj, "guiId"), None);
        assert_eq!(json_str(&obj, "count"), None);
    }

    #[test]
    fn json_str_normalized_cases() {
        assert_eq!(
            json_str_normalized(&json!({"name": "Christmas    "}), "name").as_deref(),
            Some("christmas")
        );
        assert_eq!(json_str_normalized(&json!({"name": ""}), "name"), None);
        assert_eq!(json_str_normalized(&json!({"name": "  "}), "name"), None);
        assert_eq!(json_str_normalized(&json!({}), "name"), None);
    }

    #[test]
    fn lenient_parse_ignores_trailing_nul() {
        assert_eq!(parse_json_lenient("{\"a\":1}\0").unwrap(), json!({"a": 1}));
        assert!(parse_json_lenient("{\"a\":").is_err());
    }

    #[test]
    fn build_path_never_escapes_base() {
        assert_eq!(build_path("base/", "child"), PathBuf::from("base/child"));
        assert_eq!(build_path("base", "/etc"), PathBuf::from("base//etc"));
    }

    #[test]
    fn human_readable() {
        assert_eq!(to_human_readable("front_door"), "Front Door");
        assert_eq!(to_human_readable("back-door"), "Back Door");
    }
}
