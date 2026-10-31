use doorbell_mqtt_unifi::command::{Ctx, Handler};
use doorbell_mqtt_unifi::config::{self, MqttConfig};
use doorbell_mqtt_unifi::ha::{self, Ha};
use doorbell_mqtt_unifi::repo::ProfilesRepo;
use doorbell_mqtt_unifi::{APP_VERSION, log_error, log_fatal, log_info, log_warn, logger, state};

use rumqttc::{
    Client, Event, LastWill, MqttOptions, Outgoing, Packet, RecvTimeoutError, Transport,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

const INBOUND_QUEUE_CAP: usize = 64;
const RECONNECT_DELAY: Duration = Duration::from_secs(5);
const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(10);

fn env_path(name: &str, default: &str) -> PathBuf {
    PathBuf::from(std::env::var(name).unwrap_or_else(|_| default.to_string()))
}

fn print_banner() {
    log_info!("UniFi Doorbell MQTT Service v{}", APP_VERSION);
    log_info!("ChrisHansenTech — https://chrishansen.tech");
    log_info!("Source: https://github.com/ChrisHansenTech/doorbell-mqtt-unifi");
    log_info!("License: MIT");
}

fn mqtt_options(cfg: &MqttConfig) -> Option<MqttOptions> {
    let mut opts = MqttOptions::new(&cfg.client_id, &cfg.host, cfg.port);

    // rumqttc rejects keep-alives under 5 s
    opts.set_keep_alive(Duration::from_secs(cfg.keepalive.max(5) as u64));
    opts.set_clean_session(cfg.clean_session != 0);

    if !cfg.username.is_empty() || !cfg.password.is_empty() {
        opts.set_credentials(&cfg.username, &cfg.password);
    }

    opts.set_last_will(LastWill::new(
        ha::availability_topic(cfg),
        "offline",
        ha::qos(cfg.qos),
        cfg.retained_online != 0,
    ));

    if cfg.tls_enabled != 0 {
        if cfg.cafile.is_empty() {
            log_error!("MQTT_TLS_ENABLED=1 but MQTT_CAFILE not set.");
            return None;
        }

        let read = |p: &str| {
            std::fs::read(p)
                .map_err(|e| log_error!("Failed to read '{}': {}", p, e))
                .ok()
        };
        let ca = read(&cfg.cafile)?;
        let client_auth = match (cfg.certfile.is_empty(), cfg.keyfile.is_empty()) {
            (false, false) => Some((read(&cfg.certfile)?, read(&cfg.keyfile)?)),
            _ => None,
        };

        if !cfg.keypass.is_empty() {
            // ponytail: rustls only loads unencrypted PEM keys; decrypt the key file if this matters
            log_warn!(
                "MQTT_KEYPASS is set but encrypted private keys are not supported; the key must be unencrypted."
            );
        }

        opts.set_transport(Transport::tls(ca, client_auth, None));
    }

    Some(opts)
}

struct Inbound {
    topic: String,
    payload: String,
}

/// Commands run one at a time on a worker thread, so a slow SSH apply never stalls MQTT keep-alives.
fn start_router(
    ctx: Arc<Ctx>,
    shutdown: Arc<AtomicBool>,
) -> (SyncSender<Inbound>, thread::JoinHandle<()>) {
    let routes: HashMap<String, Handler> = ha::command_routes(&ctx.ha).into_iter().collect();
    let (tx, rx) = mpsc::sync_channel::<Inbound>(INBOUND_QUEUE_CAP);

    let worker = thread::spawn(move || {
        while let Ok(msg) = rx.recv() {
            // Queued commands are dropped on shutdown
            if shutdown.load(Ordering::SeqCst) {
                break;
            }

            doorbell_mqtt_unifi::log_debug!("{}: {}", msg.topic, msg.payload);

            match routes.get(&msg.topic) {
                Some(handler) => handler(&ctx, &msg.payload),
                None => log_warn!("Unknown topic: '{}'", msg.topic),
            }
        }
    });

    (tx, worker)
}

fn main() {
    let shutdown = Arc::new(AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(sig, Arc::clone(&shutdown))
            .expect("registering signal handler");
    }

    logger::init();
    print_banner();

    let config_path = env_path("DOORBELL_CONFIG_PATH", "/config/config.json");
    let state_dir = env_path("DOORBELL_STATE_DIR", "/config/state");
    let profiles_dir = env_path("DOORBELL_PROFILES_DIR", "/profiles");
    let sounds_dir = env_path("DOORBELL_SOUNDS_DIR", "/sounds");

    state::migrate_last_applied(&profiles_dir, &state_dir);

    match std::fs::remove_dir_all("/tmp/doorbell-mqtt-unifi") {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            log_warn!("Did not clean '/tmp/doorbell-mqtt-unifi': {}", e)
        }
        _ => {}
    }

    let Some(cfg) = config::load(&config_path) else {
        log_fatal!("Configuration load failed. Exiting.");
        std::process::exit(1);
    };

    let Some(repo) = ProfilesRepo::init(&profiles_dir) else {
        log_fatal!("Profiles initialization failed. Exiting.");
        std::process::exit(1);
    };

    let Some(opts) = mqtt_options(&cfg.mqtt) else {
        log_fatal!("MQTT initialization failed. Exiting.");
        std::process::exit(1);
    };

    // ponytail: large request queue so on_connect (discovery + subscriptions) never fills it
    let (client, mut connection) = Client::new(opts, 512);
    let address = cfg.mqtt.address.clone();
    let client_id = cfg.mqtt.client_id.clone();

    let ctx = Arc::new(Ctx {
        ha: Ha::new(client.clone(), &cfg.mqtt),
        cfg,
        repo,
        state_dir,
        sounds_dir,
    });

    // Embedded web interface: configures the doorbell from a phone or desktop
    // browser. Independent of MQTT; runs until the process exits.
    let web_enabled = match std::env::var("DOORBELL_WEB_ENABLED") {
        Ok(v) => !v.eq_ignore_ascii_case("0") && !v.eq_ignore_ascii_case("false"),
        Err(_) => true,
    };
    if web_enabled {
        let bind = std::env::var("DOORBELL_WEB_BIND").unwrap_or_else(|_| "0.0.0.0".to_string());
        let port: u16 = std::env::var("DOORBELL_WEB_PORT")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(8080);
        let web_ctx = Arc::clone(&ctx);
        let web_config_path = config_path.clone();
        let web_profiles_dir = profiles_dir.clone();
        let web_bind = bind.clone();
        std::thread::Builder::new()
            .name("web-ui".into())
            .spawn(move || {
                doorbell_mqtt_unifi::web::run(
                    web_ctx,
                    web_config_path,
                    web_profiles_dir,
                    &web_bind,
                    port,
                );
            })
            .expect("spawning web UI thread");
        log_info!("Web interface enabled on http://{bind}:{port}");
    }

    let (tx, worker) = start_router(Arc::clone(&ctx), Arc::clone(&shutdown));

    let mut connected = false;
    let mut ever_connected = false;

    while !shutdown.load(Ordering::SeqCst) {
        match connection.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(Event::Incoming(Packet::ConnAck(_)))) => {
                if ever_connected {
                    log_info!("Reconnected: {} (client_id={})", address, client_id);
                } else {
                    log_info!("Connected: {} (client_id={})", address, client_id);
                }
                connected = true;
                ever_connected = true;
                ha::on_connect(&ctx);
            }
            Ok(Ok(Event::Incoming(Packet::Publish(p)))) => {
                let msg = Inbound {
                    topic: p.topic,
                    payload: String::from_utf8_lossy(&p.payload).into_owned(),
                };
                if let Err(TrySendError::Full(m) | TrySendError::Disconnected(m)) = tx.try_send(msg)
                {
                    log_warn!("Inbound queue is full, dropping message on '{}'", m.topic);
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                if connected {
                    log_warn!("Connection lost: {}", e);
                } else {
                    log_error!(
                        "MQTT {} failed to {}: {}",
                        if ever_connected {
                            "reconnect"
                        } else {
                            "connect"
                        },
                        address,
                        e
                    );
                }
                connected = false;

                let retry_at = Instant::now() + RECONNECT_DELAY;
                while Instant::now() < retry_at && !shutdown.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(100));
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    log_info!("Shutdown requested, stopping service...");

    drop(tx);
    let _ = worker.join();

    if connected {
        ctx.ha.set_availability(false);
        let _ = client.disconnect();

        // Let the event loop flush the offline message and the DISCONNECT packet
        let deadline = Instant::now() + DISCONNECT_TIMEOUT;
        while Instant::now() < deadline {
            match connection.recv_timeout(Duration::from_millis(100)) {
                Ok(Ok(Event::Outgoing(Outgoing::Disconnect)))
                | Ok(Err(_))
                | Err(RecvTimeoutError::Disconnected) => break,
                _ => {}
            }
        }
    }

    log_info!("Service stopped cleanly.");
}
