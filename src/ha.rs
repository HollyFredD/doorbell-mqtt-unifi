//! Home Assistant side: topics, MQTT discovery and the status entities.

use crate::APP_VERSION;
use crate::command::{self, Ctx, Handler};
use crate::config::{Config, MqttConfig};
use crate::errors::*;
use crate::state;

use rumqttc::{Client, QoS};
use serde_json::{Map, Value, json};
use std::sync::Mutex;

pub fn qos(n: i32) -> QoS {
    match n {
        0 => QoS::AtMostOnce,
        2 => QoS::ExactlyOnce,
        _ => QoS::AtLeastOnce,
    }
}

/// Publishes the service's state under `<prefix>/doorbell-mqtt/<instance>/...`.
pub struct Ha {
    client: Client,
    subscribe_qos: QoS,
    base: String,
    last_error: Mutex<(i32, String)>,
}

impl Ha {
    pub fn new(client: Client, cfg: &MqttConfig) -> Ha {
        Ha {
            client,
            subscribe_qos: qos(cfg.qos),
            base: base_topic(cfg),
            last_error: Mutex::new((0, String::new())),
        }
    }

    pub fn topic(&self, subtopic: &str) -> String {
        format!("{}/{}", self.base, subtopic)
    }

    /// Retained, QoS 1. Never blocks: while the broker is unreachable messages are
    /// queued by the client, and dropped with an error if the queue is full.
    pub fn publish(&self, topic: &str, payload: &str) {
        if let Err(e) =
            self.client
                .try_publish(topic, QoS::AtLeastOnce, true, payload.as_bytes().to_vec())
        {
            log_error!("Publish failed topic={}: {}", topic, e);
            return;
        }

        log_debug!("Publishing {}", payload);

        const MAX_PREVIEW: usize = 56;
        let preview = match payload.char_indices().nth(MAX_PREVIEW) {
            Some((end, _)) => format!("{}...", &payload[..end]),
            None => payload.to_string(),
        };
        log_info!("Published '{}' -> {}", preview, topic);
    }

    pub fn subscribe(&self, topic: &str) {
        match self.client.try_subscribe(topic, self.subscribe_qos) {
            Ok(()) => log_info!("Subscribed: {} (qos={})", topic, self.subscribe_qos as u8),
            Err(e) => log_error!("Subscribe failed topic={}: {}", topic, e),
        }
    }

    fn publish_sub(&self, subtopic: &str, payload: &str) {
        self.publish(&self.topic(subtopic), payload);
    }

    pub fn set_state(&self, state: &str) {
        self.publish_sub("status", state);
    }

    fn publish_error(&self, code: i32, message: &str) {
        let payload = json!({ "code": code, "name": code_name(code), "message": message });
        self.publish_sub("last_error", &payload.to_string());
    }

    /// Records the error, logs it when it differs from the previous one, publishes `last_error`.
    pub fn set_error(&self, code: i32, message: &str) {
        let mut last = self.last_error.lock().unwrap_or_else(|e| e.into_inner());
        if last.0 != code || last.1 != message {
            *last = (code, message.to_string());
            log_error!("code={} name={} msg={}", code, code_name(code), message);
        }
        drop(last);

        self.publish_error(code, message);
    }

    /// "<default message for code>: <detail>"
    pub fn err(&self, code: i32, detail: &str) {
        let base = default_message(code);
        if detail.is_empty() {
            self.set_error(code, base);
        } else {
            self.set_error(code, &format!("{base}: {detail}"));
        }
    }

    pub fn app_err(&self, e: &AppError) {
        self.err(e.code, &e.message);
    }

    pub fn clear_error(&self) {
        *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = (0, String::new());
        self.publish_error(ERROR_NONE, "");
    }

    /// Last error code and message (0/empty when the last operation succeeded).
    pub fn last_error(&self) -> (i32, String) {
        let g = self.last_error.lock().unwrap_or_else(|e| e.into_inner());
        (g.0, g.1.clone())
    }

    pub fn set_last_applied_profile(&self, profile: &str) {
        self.publish_sub("last_applied_profile", profile);
    }

    pub fn set_preset_selected(&self, preset: &str) {
        self.publish_sub("preset/selected", preset);
    }

    pub fn set_sfx_preset_selected(&self, preset: &str) {
        self.publish_sub("sfxpreset/selected", preset);
    }

    pub fn set_custom_directory(&self, directory: &str) {
        self.publish_sub("custom/directory", directory);
    }

    pub fn set_last_download(&self, directory: &str, path: &str, timestamp: &str) {
        self.publish_sub("last_download/time/state", timestamp);
        let attrs = json!({ "directory": directory, "full_path": path });
        self.publish_sub("last_download/time/attributes", &attrs.to_string());
    }

    pub fn set_validate_profile(
        &self,
        state: &str,
        profile: &str,
        expected_hash: &str,
        applied_hash: &str,
        timestamp: &str,
    ) {
        self.publish_sub("profile_validation", state);
        let attrs = json!({
            "profile": profile,
            "expected_hash": expected_hash,
            "applied_hash": applied_hash,
            "last_validated": timestamp,
        });
        self.publish_sub("profile_validation/attributes", &attrs.to_string());
    }

    pub fn set_availability(&self, available: bool) {
        self.publish_sub("availability", if available { "online" } else { "offline" });
    }
}

pub fn base_topic(cfg: &MqttConfig) -> String {
    format!("{}/doorbell-mqtt/{}", cfg.prefix, cfg.instance)
}

pub fn availability_topic(cfg: &MqttConfig) -> String {
    format!("{}/availability", base_topic(cfg))
}

// ---- Entities ----

pub struct Entity {
    pub component: &'static str,
    pub object_id: &'static str,
    pub name: &'static str,
    pub category: Option<&'static str>,
    pub state_topic: Option<&'static str>,
    pub command_topic: Option<&'static str>,
    pub icon: &'static str,
    pub device_class: Option<&'static str>,
    pub value_template: Option<&'static str>,
    pub json_attributes_topic: Option<&'static str>,
    pub options: Option<fn(&Config) -> Vec<String>>,
    pub handler: Option<Handler>,
}

const BASE: Entity = Entity {
    component: "sensor",
    object_id: "",
    name: "",
    category: None,
    state_topic: None,
    command_topic: None,
    icon: "",
    device_class: None,
    value_template: None,
    json_attributes_topic: None,
    options: None,
    handler: None,
};

fn preset_options(cfg: &Config) -> Vec<String> {
    cfg.presets.iter().map(|p| p.display_name.clone()).collect()
}

fn sfx_preset_options(cfg: &Config) -> Vec<String> {
    cfg.sfx_presets.iter().map(|p| p.name.clone()).collect()
}

pub const ENTITIES: &[Entity] = &[
    Entity {
        object_id: "status",
        name: "Status",
        category: Some("diagnostic"),
        state_topic: Some("status"),
        icon: "mdi:information-outline",
        ..BASE
    },
    Entity {
        object_id: "last_error",
        name: "Last error",
        category: Some("diagnostic"),
        state_topic: Some("last_error"),
        icon: "mdi:alert-circle-outline",
        value_template: Some("{{ value_json.message }}"),
        json_attributes_topic: Some("last_error"),
        ..BASE
    },
    Entity {
        object_id: "last_applied_profile",
        name: "Last applied profile",
        state_topic: Some("last_applied_profile"),
        icon: "mdi:badge-account",
        ..BASE
    },
    Entity {
        object_id: "profile_validation",
        name: "Profile validation",
        category: Some("diagnostic"),
        state_topic: Some("profile_validation"),
        icon: "mdi:shield-check",
        json_attributes_topic: Some("profile_validation/attributes"),
        ..BASE
    },
    Entity {
        component: "select",
        object_id: "preset",
        name: "Presets",
        category: Some("config"),
        state_topic: Some("preset/selected"),
        command_topic: Some("cmd/preset_set"),
        icon: "mdi:tune-variant",
        options: Some(preset_options),
        handler: Some(command::set_preset),
        ..BASE
    },
    Entity {
        component: "text",
        object_id: "custom_directory",
        name: "Custom directory",
        category: Some("config"),
        state_topic: Some("custom/directory"),
        command_topic: Some("cmd/apply_custom"),
        icon: "mdi:folder",
        handler: Some(command::apply_custom),
        ..BASE
    },
    Entity {
        component: "button",
        object_id: "test_config",
        name: "Test config",
        category: Some("config"),
        command_topic: Some("cmd/test_config"),
        icon: "mdi:test-tube",
        handler: Some(command::test_config),
        ..BASE
    },
    Entity {
        component: "button",
        object_id: "download_assets",
        name: "Asset download",
        command_topic: Some("cmd/download_assets"),
        icon: "mdi:download",
        handler: Some(command::download_assets),
        ..BASE
    },
    Entity {
        component: "button",
        object_id: "profile_validate",
        name: "Validate profile",
        category: Some("config"),
        command_topic: Some("cmd/validate_profile"),
        icon: "mdi:check-circle-outline",
        handler: Some(command::validate_profile),
        ..BASE
    },
    Entity {
        component: "button",
        object_id: "reapply_last_profile",
        name: "Reapply last profile",
        category: Some("config"),
        command_topic: Some("cmd/reapply_last_profile"),
        icon: "mdi:restore",
        handler: Some(command::reapply_last_profile),
        ..BASE
    },
    Entity {
        object_id: "last_download",
        name: "Last asset download",
        category: Some("diagnostic"),
        state_topic: Some("last_download/time/state"),
        icon: "mdi:folder-arrow-down",
        device_class: Some("timestamp"),
        json_attributes_topic: Some("last_download/time/attributes"),
        ..BASE
    },
    Entity {
        component: "select",
        object_id: "sfxpreset",
        name: "SFX presets",
        state_topic: Some("sfxpreset/selected"),
        command_topic: Some("cmd/sfx_preset_play"),
        icon: "mdi:bullhorn",
        options: Some(sfx_preset_options),
        handler: Some(command::play_sfx_preset),
        ..BASE
    },
];

/// Command topics and their handlers, for the router.
pub fn command_routes(ha: &Ha) -> Vec<(String, Handler)> {
    ENTITIES
        .iter()
        .filter_map(|e| Some((ha.topic(e.command_topic?), e.handler?)))
        .collect()
}

// ---- Discovery ----

const SLUG: &str = "doorbell_mqtt_unifi";
const SERVICE_NAME: &str = "UniFi Doorbell MQTT Service";

fn entity_name(name: &str, instance_human: &str) -> String {
    if instance_human.eq_ignore_ascii_case("default") {
        name.to_string()
    } else {
        format!("{name} ({instance_human})")
    }
}

fn discovery_payload(e: &Entity, cfg: &Config) -> Value {
    let m = &cfg.mqtt;
    let base = base_topic(m);
    let topic = |sub: &str| format!("{base}/{sub}");
    let default_instance = m.instance == "default";

    let mut root = Map::new();
    let mut put = |k: &str, v: Value| {
        root.insert(k.to_string(), v);
    };

    put("name", entity_name(e.name, &m.instance_human).into());
    if let Some(c) = e.category {
        put("entity_category", c.into());
    }
    put("object_id", e.object_id.into());

    let unique_id = if default_instance {
        format!("{SLUG}_{}", e.object_id)
    } else {
        format!("{SLUG}_{}_{}", m.instance, e.object_id)
    };
    put("unique_id", unique_id.into());

    let default_entity_id = if default_instance {
        format!("{}.{SLUG}_{}", e.component, e.object_id)
    } else {
        format!("{}.{SLUG}_{}_{}", e.component, e.object_id, m.instance)
    };
    put("default_entity_id", default_entity_id.into());

    if let Some(t) = e.state_topic {
        put("state_topic", topic(t).into());
    }
    put("availability_topic", topic("availability").into());
    if let Some(t) = e.command_topic {
        put("command_topic", topic(t).into());
    }
    if let Some(options) = e.options {
        put("options", options(cfg).into());
    }
    put("icon", e.icon.into());
    if let Some(dc) = e.device_class {
        put("device_class", dc.into());
    }
    if let Some(vt) = e.value_template {
        put("value_template", vt.into());
    }
    if let Some(t) = e.json_attributes_topic {
        put("json_attributes_topic", topic(t).into());
    }
    put(
        "device",
        json!({
            "identifiers": format!("{}_doorbell_mqtt_{}", m.prefix, m.instance),
            "manufacturer": "ChrisHansenTech",
            "model": SERVICE_NAME,
            "name": entity_name(SERVICE_NAME, &m.instance_human),
            "sw_version": APP_VERSION,
        }),
    );
    put(
        "origin",
        json!({
            "name": SERVICE_NAME,
            "sw_version": APP_VERSION,
            "url": "https://github.com/ChrisHansenTech/doorbell-mqtt-unifi",
        }),
    );

    Value::Object(root)
}

pub fn publish_discovery(ha: &Ha, cfg: &Config) {
    for e in ENTITIES {
        let topic = format!(
            "homeassistant/{}/{}_doorbell_mqtt_{}_{}/config",
            e.component, cfg.mqtt.prefix, cfg.mqtt.instance, e.object_id
        );
        ha.publish(&topic, &discovery_payload(e, cfg).to_string());
    }
}

/// On every (re)connect: discovery, command subscriptions, restore the last applied state, online.
pub fn on_connect(ctx: &Ctx) {
    publish_discovery(&ctx.ha, &ctx.cfg);

    for (topic, _) in command_routes(&ctx.ha) {
        ctx.ha.subscribe(&topic);
    }

    match state::load(&ctx.state_dir) {
        Ok(last) => {
            ctx.ha.set_last_applied_profile(&last.profile_name);
            ctx.ha.set_preset_selected(if last.is_preset {
                &last.profile_name
            } else {
                "none"
            });
            ctx.ha.set_custom_directory("");
        }
        Err(_) => log_warn!("Failed to load last_applied.json, state will not be restored"),
    }

    ctx.ha.set_availability(true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn discovery_payload_shape() {
        let cfg =
            crate::config::load(Path::new("tests/fixtures/config/config_valid.json")).unwrap();

        let select = discovery_payload(&ENTITIES[4], &cfg);
        assert_eq!(
            select["options"],
            json!(["Christmas", "New Years", "St. Patrick's Day"])
        );
        assert!(
            select["command_topic"]
                .as_str()
                .unwrap()
                .ends_with("/cmd/preset_set")
        );

        // Buttons have no state topic
        let button = discovery_payload(&ENTITIES[6], &cfg);
        assert!(button.get("state_topic").is_none());

        let keys: Vec<_> = discovery_payload(&ENTITIES[1], &cfg)
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(keys.first().map(String::as_str), Some("name"));
        assert_eq!(keys.last().map(String::as_str), Some("origin"));
    }
}
