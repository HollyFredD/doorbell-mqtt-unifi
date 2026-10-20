# Getting Started

This guide walks through deploying Doorbell MQTT UniFi using Docker.

## Requirements

- Home Assistant with MQTT integration enabled
- MQTT broker (Mosquitto or equivalent)
- UniFi Protect G4 Doorbell Pro
- SSH enabled for the doorbell

---

## Step 1: Run the Container

```bash
docker run -d \
  --name doorbell-mqtt-unifi \
  --restart unless-stopped \
  -e UNIFI_PROTECT_RECOVERY_CODE=your_recovery_code \
  -v /path/to/doorbell-mqtt-unifi/config:/config \
  -v /path/to/doorbell-mqtt-unifi/profiles:/profiles \
  ghcr.io/chrishansentech/doorbell-mqtt-unifi:latest
```

---

## Step 2: Configure

Until `mqtt.host`, `ssh.host` and the recovery code are set (in `config.json`, or with `-e`),
the container waits and logs `Waiting for configuration`.

Run the interactive setup:

```bash
docker exec -it doorbell-mqtt-unifi config
```

It asks for the MQTT broker, the doorbell address, the recovery code and the apply method.
Values already in `config.json` are shown and kept when you press Enter, and values set
with environment variables are skipped. The recovery code is saved in `/config/secrets.env`.

The service starts as soon as the configuration is complete. If it was already running, restart it:

```bash
docker restart doorbell-mqtt-unifi
```

You can also edit `/path/to/doorbell-mqtt-unifi/config/config.json` by hand, then restart the container.

---

## Step 3: Verify in Home Assistant

After restart, the device and entities should appear automatically via MQTT discovery.
