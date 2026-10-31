use crate::utils::{self, FileClass};

use serde_json::{Value, json};
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ApplyMethod {
    Legacy,
    Ipc,
}

#[derive(Clone, Debug, Default)]
pub struct MqttConfig {
    pub host: String,
    pub port: u16,
    /// "tcp://host:port" or "ssl://host:port", for logs.
    pub address: String,
    pub client_id: String,
    pub username: String,
    pub password: String,
    pub qos: i32,
    pub keepalive: i32,
    pub clean_session: i32,
    pub retained_online: i32,

    pub tls_enabled: i32,
    pub cafile: String,
    pub certfile: String,
    pub keyfile: String,
    pub keypass: String,

    pub prefix: String,
    pub instance: String,
    pub instance_human: String,
}

#[derive(Clone, Debug, Default)]
pub struct SshConfig {
    pub host: String,
    pub port: i32,
    pub user: String,
    pub password_env: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub display_name: String,
    pub key_name: String,
    pub directory: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SfxPreset {
    pub name: String,
    pub key_name: String,
    pub file: String,
    pub volume: i32,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub mqtt: MqttConfig,
    pub ssh: SshConfig,
    pub apply_method: ApplyMethod,
    pub presets: Vec<Preset>,
    pub sfx_presets: Vec<SfxPreset>,
    pub sfx_default_volume: i32,
}

#[derive(PartialEq)]
enum Source {
    Env,
    Config,
    Default,
    None,
}

/// A string setting: non-empty env var, else non-empty JSON string, else default.
/// `max_len` is the longest accepted value in bytes; longer values fail the load.
struct StrSetting<'a> {
    json_key: Option<&'a str>,
    env_name: Option<&'a str>,
    default: &'a str,
    label: &'a str,
    max_len: usize,
    secret: bool,
}

impl StrSetting<'_> {
    fn load(&self, root: Option<&Value>) -> Option<String> {
        let name = self.label;
        let mut source = Source::None;
        let mut value: Option<String> = None;

        if let Some(env_name) = self.env_name
            && let Ok(v) = std::env::var(env_name)
            && !v.is_empty()
        {
            value = Some(v);
            source = Source::Env;
        }

        if value.is_none()
            && let (Some(root), Some(key)) = (root, self.json_key)
            && let Some(v) = utils::json_str(root, key).filter(|v| !v.is_empty())
        {
            value = Some(v.to_string());
            source = Source::Config;
        }

        let value = value.unwrap_or_else(|| {
            source = Source::Default;
            self.default.to_string()
        });

        if value.len() > self.max_len {
            log_error!(
                "Configuration value for '{}' exceeds maximum length ({}).",
                name,
                self.max_len
            );
            return None;
        }

        let env_name = self.env_name.unwrap_or("");
        let json_key = self.json_key.unwrap_or("");
        match (self.secret, &source) {
            (true, Source::Env) => log_debug!(
                "{} configured from environment variable '{}'.",
                name,
                env_name
            ),
            (true, Source::Config) => {
                log_debug!("{} configured from JSON key '{}'.", name, json_key)
            }
            (true, _) => log_debug!("{} configured from default value.", name),
            (false, Source::Env) => log_debug!("{}='{}' (from env '{}').", name, value, env_name),
            (false, Source::Config) => {
                log_debug!("{}='{}' (from JSON key '{}').", name, value, json_key)
            }
            (false, _) => log_debug!("{}='{}' (from default).", name, value),
        }

        if source == Source::Default && value == "localhost" {
            if name == "mqtt.host" {
                log_warn!(
                    "{} not set; defaulting to '{}'. When running in Docker, '{}' refers to the container itself. \
                     Set MQTT_HOST or update /config/config.json and restart.",
                    name,
                    value,
                    value
                );
            } else if name == "ssh.host" {
                log_warn!(
                    "ssh.host not set; defaulting to '{}'. Set SSH_HOST or update /config/config.json and restart.",
                    value
                );
            }
        }

        Some(value)
    }
}

fn str_setting(
    root: Option<&Value>,
    json_key: &str,
    env_name: Option<&str>,
    default: &str,
    label: &str,
    max_len: usize,
) -> Option<String> {
    StrSetting {
        json_key: Some(json_key),
        env_name,
        default,
        label,
        max_len,
        secret: false,
    }
    .load(root)
}

fn secret_setting(
    root: &Value,
    json_key: &str,
    env_name: &str,
    label: &str,
    max_len: usize,
) -> Option<String> {
    StrSetting {
        json_key: Some(json_key),
        env_name: Some(env_name),
        default: "",
        label,
        max_len,
        secret: true,
    }
    .load(Some(root))
}

/// Env var (whole string must be an integer), else JSON number, else default.
fn int_setting(root: &Value, json_key: &str, env_name: &str, default: i32) -> i32 {
    if let Ok(v) = std::env::var(env_name)
        && let Ok(n) = v.parse::<i64>()
    {
        return n as i32;
    }

    utils::json_int(root, json_key).unwrap_or(default)
}

fn load_mqtt(root: &Value) -> Option<MqttConfig> {
    let host = str_setting(
        Some(root),
        "host",
        Some("MQTT_HOST"),
        "localhost",
        "mqtt.host",
        39,
    )?;
    let tls_enabled = int_setting(root, "tls_enabled", "MQTT_TLS_ENABLED", 0);
    let port = int_setting(root, "port", "MQTT_PORT", 1883);

    let Ok(port) = u16::try_from(port) else {
        log_error!("Invalid MQTT port {}.", port);
        return None;
    };

    let proto = if tls_enabled != 0 { "ssl" } else { "tcp" };
    let address = format!("{proto}://{host}:{port}");

    let prefix = str_setting(
        Some(root),
        "prefix",
        Some("MQTT_PREFIX"),
        "chrishansentech",
        "mqtt.prefix",
        29,
    )?;
    let instance = str_setting(
        Some(root),
        "instance",
        Some("MQTT_INSTANCE"),
        "default",
        "mqtt.instance",
        63,
    )?;
    let instance_human = utils::to_human_readable(&instance);

    let default_client_id = format!("{prefix}-doorbell-mqtt-unifi-{instance}");
    let client_id = StrSetting {
        json_key: None,
        env_name: Some("MQTT_CLIENT_ID"),
        default: &default_client_id,
        label: "mqtt.client_id",
        max_len: 255,
        secret: false,
    }
    .load(None)?;

    let cfg = MqttConfig {
        host,
        port,
        address,
        client_id,
        username: str_setting(
            Some(root),
            "username",
            Some("MQTT_USERNAME"),
            "",
            "mqtt.username",
            127,
        )?,
        password: secret_setting(root, "password", "MQTT_PASSWORD", "mqtt.password", 127)?,
        qos: int_setting(root, "qos", "MQTT_QOS", 1),
        keepalive: int_setting(root, "keepalive", "MQTT_KEEPALIVE", 30),
        clean_session: int_setting(root, "clean_session", "MQTT_CLEAN_SESSION", 1),
        retained_online: int_setting(root, "retained_online", "MQTT_RETAINED_ONLINE", 1),
        tls_enabled,
        cafile: str_setting(
            Some(root),
            "cafile",
            Some("MQTT_CAFILE"),
            "",
            "mqtt.cafile",
            255,
        )?,
        certfile: str_setting(
            Some(root),
            "certfile",
            Some("MQTT_CERT_FILE"),
            "",
            "mqtt.certfile",
            255,
        )?,
        keyfile: str_setting(
            Some(root),
            "keyfile",
            Some("MQTT_KEY_FILE"),
            "",
            "mqtt.keyfile",
            255,
        )?,
        keypass: secret_setting(root, "keypass", "MQTT_KEYPASS", "mqtt.keypass", 127)?,
        prefix,
        instance,
        instance_human,
    };

    log_debug!("MQTT address: {}", cfg.address);
    log_debug!(
        "MQTT prefix: '{}', instance: '{}' (human: '{}')",
        cfg.prefix,
        cfg.instance,
        cfg.instance_human
    );
    log_debug!(
        "MQTT QoS={} keepalive={} clean_session={} retained_online={}",
        cfg.qos,
        cfg.keepalive,
        cfg.clean_session,
        cfg.retained_online
    );

    Some(cfg)
}

fn load_ssh(root: &Value) -> Option<SshConfig> {
    Some(SshConfig {
        host: str_setting(
            Some(root),
            "host",
            Some("SSH_HOST"),
            "localhost",
            "ssh.host",
            255,
        )?,
        user: str_setting(
            Some(root),
            "username",
            Some("SSH_USERNAME"),
            "ubnt",
            "ssh.username",
            29,
        )?,
        password_env: str_setting(
            Some(root),
            "password_env",
            None,
            "UNIFI_PROTECT_RECOVERY_CODE",
            "ssh.password_env",
            49,
        )?,
        port: int_setting(root, "port", "SSH_PORT", 22),
    })
}

fn load_unifi(unifi: &Value) -> Option<ApplyMethod> {
    let method = str_setting(
        Some(unifi),
        "apply_method",
        Some("UNIFI_APPLY_METHOD"),
        "IPC",
        "unifi.apply_method",
        29,
    )?;

    if method.eq_ignore_ascii_case("legacy") {
        Some(ApplyMethod::Legacy)
    } else if method.eq_ignore_ascii_case("ipc") {
        Some(ApplyMethod::Ipc)
    } else {
        log_error!(
            "Invalid unifi.apply_method '{}' (expected 'ipc' or 'legacy').",
            method
        );
        None
    }
}

fn load_presets(presets: &[Value]) -> Vec<Preset> {
    if presets.is_empty() {
        log_warn!("No presets configured in the 'presets' section");
        return Vec::new();
    }

    let mut out: Vec<Preset> = Vec::new();

    for (i, item) in presets.iter().enumerate() {
        let (Some(name), Some(directory)) = (
            utils::json_str(item, "name"),
            utils::json_str(item, "directory"),
        ) else {
            log_warn!(
                "Invalid preset entry at index {} (missing name or directory).",
                i
            );
            continue;
        };

        if !utils::is_valid_directory_name(directory) {
            log_warn!(
                "Invalid preset directory name '{}' for preset '{}'",
                directory,
                name
            );
            continue;
        }

        let Some(key_name) = utils::json_str_normalized(item, "name") else {
            log_warn!("Invalid preset entry at index {} (blank name).", i);
            continue;
        };

        if out.iter().any(|p| p.key_name == key_name) {
            log_warn!(
                "Duplicate preset name (case/space-insensitive) '{}' at index {}",
                name,
                i
            );
            continue;
        }

        out.push(Preset {
            display_name: name.to_string(),
            key_name,
            directory: directory.to_string(),
        });
    }

    log_info!("Loaded {} preset mappings from configuration.", out.len());
    out
}

fn load_sfx_presets(presets: &[Value], default_volume: i32) -> Vec<SfxPreset> {
    if presets.is_empty() {
        log_warn!("No presets configured in the 'sfx.presets' section");
        return Vec::new();
    }

    let mut out: Vec<SfxPreset> = Vec::new();

    for (i, item) in presets.iter().enumerate() {
        let volume = utils::json_int(item, "volume")
            .filter(|v| (1..=100).contains(v))
            .unwrap_or(default_volume);

        let (Some(name), Some(file)) =
            (utils::json_str(item, "name"), utils::json_str(item, "file"))
        else {
            log_warn!(
                "Invalid sfx preset entry at index {} (missing name or file).",
                i
            );
            continue;
        };

        if utils::is_valid_filename(file, FileClass::Sound).is_err() {
            log_warn!("Invalid file '{}' in SFX preset '{}'", file, name);
            continue;
        }

        let Some(key_name) = utils::json_str_normalized(item, "name") else {
            log_warn!("Invalid sfx preset entry at index {} (blank name).", i);
            continue;
        };

        if out.iter().any(|p| p.key_name == key_name) {
            log_warn!(
                "Duplicate preset name (case/space-insensitive) '{}' at index {}",
                name,
                i
            );
            continue;
        }

        out.push(SfxPreset {
            name: name.to_string(),
            key_name,
            file: file.to_string(),
            volume,
        });
    }

    log_info!(
        "Loaded {} sfx preset mappings from configuration.",
        out.len()
    );
    out
}

/// Loads config.json; env vars override JSON values. Errors are logged, None means "do not start".
pub fn load(path: &Path) -> Option<Config> {
    log_info!("Loading configuration file: {}", path.display());

    let root = utils::read_json_file(path).or_else(|| {
        log_error!("Failed to read configuration file: {}", path.display());
        None
    })?;

    let Some(mqtt) = utils::get_ci(&root, "mqtt").filter(|v| v.is_object()) else {
        log_error!("Missing or invalid 'mqtt' section in '{}'", path.display());
        return None;
    };
    let mqtt = load_mqtt(mqtt)?;

    let default_unifi = json!({ "apply_method": "legacy" });
    let unifi = match utils::get_ci(&root, "unifi") {
        Some(u) if u.is_object() => u,
        Some(_) => {
            log_error!("'unifi' must be an object in '{}'", path.display());
            return None;
        }
        None => {
            log_warn!(
                "Missing 'unifi' section in '{}'. Defaulting to 'legacy' apply method",
                path.display()
            );
            &default_unifi
        }
    };
    let apply_method = load_unifi(unifi)?;

    let Some(ssh) = utils::get_ci(&root, "ssh").filter(|v| v.is_object()) else {
        log_error!("Missing or invalid 'ssh' section in '{}'", path.display());
        return None;
    };
    let ssh = load_ssh(ssh)?;

    let presets = match utils::get_ci(&root, "presets").and_then(Value::as_array) {
        Some(p) => p.as_slice(),
        None => {
            log_warn!(
                "Missing or invalid 'presets' section in '{}'",
                path.display()
            );
            &[]
        }
    };
    let presets = load_presets(presets);

    let default_sfx = json!({ "defaultVolume": 100, "presets": [] });
    let sfx = match utils::get_ci(&root, "sfx") {
        Some(s) if s.is_object() => s,
        _ => {
            log_warn!("Missing or invalid 'sfx' section in '{}'.", path.display());
            &default_sfx
        }
    };
    let sfx_default_volume = int_setting(sfx, "defaultVolume", "SFX_DEFAULT_VOLUME", 100);
    let sfx_presets = match utils::get_ci(sfx, "presets").and_then(Value::as_array) {
        Some(p) => load_sfx_presets(p, sfx_default_volume),
        None => Vec::new(),
    };

    log_info!("Configuration loaded successfully from {}", path.display());

    Some(Config {
        mqtt,
        ssh,
        apply_method,
        presets,
        sfx_presets,
        sfx_default_volume,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Option<Config> {
        load(Path::new(&format!("tests/fixtures/config/{name}")))
    }

    #[test]
    fn loads_valid_json() {
        assert_eq!(
            fixture("config_valid.json").unwrap().apply_method,
            ApplyMethod::Ipc
        );
    }

    #[test]
    fn fails_on_missing_file() {
        assert!(fixture("does_not_exist.json").is_none());
    }

    #[test]
    fn fails_on_invalid_json() {
        assert!(fixture("config_invalid.json").is_none());
    }

    #[test]
    fn loads_presets() {
        assert_eq!(fixture("config_valid.json").unwrap().presets.len(), 3);
    }

    #[test]
    fn skips_invalid_preset() {
        assert_eq!(
            fixture("config_invalid_presets.json")
                .unwrap()
                .presets
                .len(),
            2
        );
    }

    #[test]
    fn keeps_first_entry_when_duplicates() {
        let cfg = fixture("config_duplicate_presets.json").unwrap();
        assert_eq!(cfg.presets.len(), 2);
        assert_eq!(cfg.presets[0].display_name, "Christmas");
    }

    #[test]
    fn fails_when_string_too_long() {
        assert!(fixture("config_invalid_long_strings.json").is_none());
    }

    #[test]
    fn fails_when_unifi_exists_but_not_an_object() {
        assert!(fixture("config_unifi_not_object.json").is_none());
    }

    #[test]
    fn defaults_to_legacy_when_unifi_missing() {
        assert_eq!(
            fixture("config_unifi_object_does_not_exist.json")
                .unwrap()
                .apply_method,
            ApplyMethod::Legacy
        );
    }

    #[test]
    fn skips_sfx_invalid_filenames() {
        let cfg = fixture("config_invalid_presets.json").unwrap();
        let names: Vec<_> = cfg.sfx_presets.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Hello", "Goodbye"]);
        assert_eq!(cfg.sfx_presets[0].volume, 50);
    }
}
