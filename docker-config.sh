#!/bin/sh
# Interactive configuration: docker exec -it <container> config
# config --check exits 0 when the service has what it needs to start.
set -eu

CONFIG="${DOORBELL_CONFIG_PATH:-/config/config.json}"
SECRETS="$(dirname "$CONFIG")/secrets.env"

json_get() { jq -r "$1 // empty" "$CONFIG"; }

password_env_name() {
  name="$(json_get .ssh.password_env)"
  name="${name:-UNIFI_PROTECT_RECOVERY_CODE}"
  case "$name" in
    ''|[0-9]*|*[!A-Za-z0-9_]*) echo "Invalid ssh.password_env '$name'" >&2; exit 1 ;;
  esac
  printf '%s' "$name"
}

# Read without eval: secrets.env is writable by the app user, the entrypoint runs as root
secret_get() { [ -f "$SECRETS" ] && sed -n "s/^$1=//p" "$SECRETS" | head -n 1 || true; }

if [ ! -f "$CONFIG" ]; then
  cp /defaults/config.json "$CONFIG"
fi

if [ "${1:-}" = "--check" ]; then
  pw_env="$(password_env_name)"
  missing=""
  [ -n "${MQTT_HOST:-}" ] || [ -n "$(json_get .mqtt.host)" ] || missing="$missing mqtt.host"
  [ -n "${SSH_HOST:-}" ] || [ -n "$(json_get .ssh.host)" ] || missing="$missing ssh.host"
  eval "pw=\${$pw_env:-}"
  [ -n "$pw" ] || [ -n "$(secret_get "$pw_env")" ] || missing="$missing $pw_env"
  if [ -n "$missing" ]; then
    echo "[WARN] Missing configuration:$missing"
    exit 1
  fi
  exit 0
fi

if [ ! -t 0 ]; then
  echo "Run interactively: docker exec -it <container> config" >&2
  exit 1
fi

trap 'stty echo 2>/dev/null || true' EXIT
trap 'exit 130' INT TERM

# ask <label> <current> [secret]: prints the answer, Enter keeps the current value
ask() {
  if [ "${3:-}" = secret ]; then
    if [ -n "$2" ]; then hint="set, Enter to keep"; else hint="empty"; fi
    printf '%s [%s]: ' "$1" "$hint" >&2
    stty -echo
    IFS= read -r answer || true
    stty echo
    echo >&2
  else
    printf '%s [%s]: ' "$1" "$2" >&2
    IFS= read -r answer || true
  fi
  printf '%s' "${answer:-$2}"
}

ask_port() {
  while :; do
    port="$(ask "$1" "$2")"
    case "$port" in
      ''|*[!0-9]*) echo "Port must be a number" >&2 ;;
      *) printf '%s' "$port"; return ;;
    esac
  done
}

# ask_field <label> <json path> <env override> [secret]
ask_field() {
  eval "env_val=\${$3:-}"
  if [ -n "$env_val" ]; then
    echo "$1: set by environment variable $3" >&2
    json_get "$2"
    return
  fi
  ask "$1" "$(json_get "$2")" "${4:-}"
}

# info <line>...: explanation shown before a question
info() {
  for line in "$@"; do printf '  | %s\n' "$line" >&2; done
}

echo "Doorbell MQTT UniFi configuration ($CONFIG)"
echo "Press Enter to keep the value shown in brackets."
echo
echo "Before you start, have at hand:"
echo "  - the address and credentials of your MQTT broker (often the Mosquitto add-on of Home Assistant)"
echo "  - the IP address of the doorbell, shown in UniFi Protect > Devices > your doorbell"
echo "  - the recovery code of UniFi Protect (see the SSH section below)"
echo

echo "== MQTT broker =="
info "The broker Home Assistant uses to discover this doorbell."
echo >&2
info "IP or hostname of the broker, e.g. 192.168.1.10." \
     "With the Mosquitto add-on, it is the IP of your Home Assistant server." \
     "Do not use localhost: inside Docker it means this container."
mqtt_host="$(ask_field "Host" .mqtt.host MQTT_HOST)"
info "1883 for a standard broker, 8883 if it only accepts TLS (TLS settings: edit config.json)."
mqtt_port="$(ask_port "Port" "$(json_get ".mqtt.port // 1883")")"
info "Credentials allowed on the broker. With the Mosquitto add-on, use a Home Assistant user" \
     "or a login declared in the add-on configuration. Leave empty if the broker has no authentication."
mqtt_user="$(ask_field "Username" .mqtt.username MQTT_USERNAME)"
mqtt_pass="$(ask_field "Password" .mqtt.password MQTT_PASSWORD secret)"
info "Short name of this doorbell, e.g. front_door. It becomes the device name in" \
     "Home Assistant (Front Door). Use a different one per container if you have several doorbells." \
     "Changing it later creates a new device in Home Assistant."
mqtt_instance="$(ask_field "Instance" .mqtt.instance MQTT_INSTANCE)"
mqtt_instance="${mqtt_instance:-default}"

echo
echo "== Doorbell (SSH) =="
info "The service sends animations and sounds to the doorbell over SSH."
echo >&2
info "IP address of the G4 Doorbell Pro: UniFi Protect > Devices > select the doorbell." \
     "Give it a fixed IP (DHCP reservation) so this value does not change."
ssh_host="$(ask_field "Doorbell IP or hostname" .ssh.host SSH_HOST)"
info "Keep 22 unless you changed it."
ssh_port="$(ask_port "SSH port" "$(json_get ".ssh.port // 22")")"
info "Keep ubnt: it is the SSH user of UniFi Protect cameras."
ssh_user="$(ask_field "SSH username" ".ssh.username // \"ubnt\"" SSH_USERNAME)"

pw_env="$(password_env_name)"
eval "pw=\${$pw_env:-}"
if [ -n "$pw" ]; then
  echo "Recovery code: set by environment variable $pw_env"
  recovery=""
else
  info "SSH password of the doorbell. In UniFi Protect, open Settings > System" \
       "(Advanced on some versions) and copy the Recovery Code." \
       "It is saved in $SECRETS, readable only by the service."
  recovery="$(ask "Recovery code" "$(secret_get "$pw_env")" secret)"
fi

echo
echo "== UniFi =="
info "How animations and sounds are applied to the doorbell:" \
     "  ipc     recommended. Updates the doorbell in memory, without restarting anything." \
     "          Takes under 2 seconds. Profile validation in Home Assistant needs it." \
     "  legacy  edits the doorbell configuration files and restarts its processes." \
     "          Takes about 30 seconds. Use it only if ipc does not work on your firmware."
while :; do
  apply_method="$(ask_field "Apply method (ipc/legacy)" ".unifi.apply_method // \"ipc\"" UNIFI_APPLY_METHOD)"
  case "$apply_method" in
    ipc|IPC|legacy) break ;;
    *) echo "Expected ipc or legacy" >&2 ;;
  esac
done

# Rewrite in place (not mv) so ownership and single-file bind mounts are preserved
tmp="$(mktemp)"
jq --arg mh "$mqtt_host" --argjson mp "$mqtt_port" --arg mu "$mqtt_user" \
   --arg mw "$mqtt_pass" --arg mi "$mqtt_instance" \
   --arg sh "$ssh_host" --argjson sp "$ssh_port" --arg su "$ssh_user" \
   --arg am "$apply_method" \
   '.mqtt.host = $mh | .mqtt.port = $mp | .mqtt.username = $mu | .mqtt.password = $mw
    | .mqtt.instance = $mi
    | .ssh.host = $sh | .ssh.port = $sp | .ssh.username = $su
    | .unifi.apply_method = $am' \
   "$CONFIG" > "$tmp"
cat "$tmp" > "$CONFIG"
rm -f "$tmp"

if [ -n "$recovery" ]; then
  tmp="$(mktemp)"
  [ -f "$SECRETS" ] && grep -v "^$pw_env=" "$SECRETS" > "$tmp" || true
  printf '%s=%s\n' "$pw_env" "$recovery" >> "$tmp"
  cat "$tmp" > "$SECRETS"
  rm -f "$tmp"
  chmod 600 "$SECRETS"
  chown "${PUID:-10001}:${PGID:-10001}" "$SECRETS" 2>/dev/null || true
fi

echo
echo "Saved $CONFIG"

if [ "$(cat /proc/1/comm 2>/dev/null)" = "doorbell-mqtt-u" ]; then
  echo "The service is already running: restart the container to apply (docker restart <container>)."
else
  echo "The service will start automatically."
fi
