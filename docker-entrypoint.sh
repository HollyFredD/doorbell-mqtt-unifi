#!/bin/sh
set -eu

PUID="${PUID:-10001}"
PGID="${PGID:-10001}"

APP_USER="appuser"
APP_GROUP="appgroup"

# Create group/user if needed
if ! getent group "$APP_GROUP" >/dev/null 2>&1; then
  addgroup --gid "$PGID" "$APP_GROUP" >/dev/null 2>&1 || true
fi

if ! id "$APP_USER" >/dev/null 2>&1; then
  adduser --disabled-password --gecos "" --uid "$PUID" --gid "$PGID" "$APP_USER" >/dev/null 2>&1 || true
fi

# Ensure dirs exist
mkdir -p /config /profiles /tmp/doorbell-mqtt-unifi

# Fix ownership (only on dirs we expect to be writable)
chown -R "$PUID:$PGID" /config /profiles /tmp/doorbell-mqtt-unifi || true

if [ ! -f /config/config.json ]; then
  echo "[INFO] No config.json found, copying default"
  cp /defaults/config.json /config/config.json
  chown "$PUID:$PGID" /config/config.json || true
fi

# Wait for a complete configuration instead of exiting, so "docker exec -it <container> config" stays possible
if ! config --check; then
  echo "[WARN] Waiting for configuration. Run: docker exec -it <container> config"
  trap 'exit 0' TERM INT
  while ! config --check >/dev/null 2>&1; do
    sleep 5
  done
  trap - TERM INT
  echo "[INFO] Configuration complete, starting"
fi

# Recovery code saved by "config", unless already passed with -e
PW_ENV="$(jq -r '.ssh.password_env // "UNIFI_PROTECT_RECOVERY_CODE"' /config/config.json)"
eval "PW_VAL=\${$PW_ENV:-}"
if [ -z "$PW_VAL" ] && [ -f /config/secrets.env ]; then
  export "$PW_ENV=$(sed -n "s/^$PW_ENV=//p" /config/secrets.env | head -n 1)"
fi

# Drop privileges and exec
exec gosu "$PUID:$PGID" /app/doorbell-mqtt-unifi