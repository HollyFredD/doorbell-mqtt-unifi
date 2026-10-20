#!/bin/sh
# Interactive configuration: docker exec -it <container> config
# config --check exits 0 when the service has what it needs to start.
set -eu

CONFIG="${DOORBELL_CONFIG_PATH:-/config/config.json}"
SECRETS="$(dirname "$CONFIG")/secrets.env"

# ANSI colors, enabled only when stderr is a terminal (interactive runs and --check).
# Explanations (TXT_INFO) and the input zone (TXT_INPUT) use different colors so
# the field to type in stands out clearly from the surrounding help text.
if [ -t 2 ]; then
  ESC="$(printf '\033')"
  TXT_RESET="${ESC}[0m"
  TXT_BOLD="${ESC}[1m"
  TXT_DIM="${ESC}[2m"
  TXT_TITLE="${ESC}[1;37m"        # bold white  : main title
  TXT_HEADER="${ESC}[1;36m"       # bold cyan   : section headers  (== ... ==)
  TXT_INFO="${ESC}[36m"           # cyan        : explanations / help text
  TXT_INPUT="${ESC}[1;33m"        # bold yellow : input zone (what the user types)
  TXT_ERROR="${ESC}[1;31m"        # bold red    : errors
  TXT_OK="${ESC}[1;32m"           # bold green  : success messages
else
  TXT_RESET="" TXT_BOLD="" TXT_DIM="" TXT_TITLE="" TXT_HEADER=""
  TXT_INFO="" TXT_INPUT="" TXT_ERROR="" TXT_OK=""
fi

err() {
  printf '%s%s%s\n' "$TXT_ERROR" "$*" "$TXT_RESET" >&2
}

# header <name>: section title, visually separated from the rest
header() {
  printf '\n%s== %s ==%s\n' "$TXT_HEADER" "$1" "$TXT_RESET"
}

json_get() { jq -r "$1 // empty" "$CONFIG"; }

password_env_name() {
  name="$(json_get .ssh.password_env)"
  name="${name:-UNIFI_PROTECT_RECOVERY_CODE}"
  case "$name" in
    ''|[0-9]*|*[!A-Za-z0-9_]*) err "Invalid ssh.password_env '$name'"; exit 1 ;;
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
    err "[WARN] Missing configuration:$missing"
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
# The label and the typed field are shown in the "input zone" color; the typed text
# is echoed in the same color so it is easy to check what was entered.
ask() {
  if [ "${3:-}" = secret ]; then
    if [ -n "$2" ]; then hint="set, Enter to keep"; else hint="empty"; fi
    printf '%s> %s [%s]: %s' "$TXT_INPUT" "$1" "$hint" "$TXT_INPUT" >&2
    stty -echo
    IFS= read -r answer || true
    stty echo
    printf '%s\n' "$TXT_RESET" >&2
  else
    printf '%s> %s [%s]: %s' "$TXT_INPUT" "$1" "$2" "$TXT_INPUT" >&2
    IFS= read -r answer || true
    printf '%s' "$TXT_RESET" >&2
  fi
  printf '%s' "${answer:-$2}"
}

ask_port() {
  while :; do
    port="$(ask "$1" "$2")"
    case "$port" in
      ''|*[!0-9]*) err "Port must be a number" ;;
      *) printf '%s' "$port"; return ;;
    esac
  done
}

# ask_field <label> <json path> <env override> [secret]
ask_field() {
  eval "env_val=\${$3:-}"
  if [ -n "$env_val" ]; then
    printf '%s%s: set by environment variable %s%s\n' "$TXT_DIM" "$1" "$3" "$TXT_RESET" >&2
    json_get "$2"
    return
  fi
  ask "$1" "$(json_get "$2")" "${4:-}"
}

# info <line>...: explanation shown before a question, in the help color
info() {
  for line in "$@"; do printf '%s  | %s%s\n' "$TXT_INFO" "$line" "$TXT_RESET" >&2; done
}

printf '%sDoorbell MQTT UniFi configuration (%s)%s\n' "$TXT_TITLE" "$CONFIG" "$TXT_RESET"
printf '%sPress Enter to keep the value shown in brackets.%s\n' "$TXT_DIM" "$TXT_RESET"
printf '\n'
info "Before you start, have at hand:" \
     "  - the address and credentials of your MQTT broker (often the Mosquitto add-on of Home Assistant)" \
     "  - the IP address of the doorbell, shown in UniFi Protect > Devices > your doorbell" \
     "  - the recovery code of UniFi Protect (see the SSH section below)"

header "MQTT broker"
info "The broker Home Assistant uses to discover this doorbell."
printf '\n'
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

header "Doorbell (SSH)"
info "The service sends animations and sounds to the doorbell over SSH."
printf '\n'
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
  printf '%sRecovery code: set by environment variable %s%s\n' "$TXT_DIM" "$pw_env" "$TXT_RESET"
  recovery=""
else
  info "SSH password of the doorbell. In UniFi Protect, open Settings > System" \
       "(Advanced on some versions) and copy the Recovery Code." \
       "It is saved in $SECRETS, readable only by the service." \
       "It is shown in clear here so you can check what you type (personal setup)."
  recovery="$(ask "Recovery code" "$(secret_get "$pw_env")")"
fi

header "UniFi"
info "How animations and sounds are applied to the doorbell:" \
     "  ipc     recommended. Updates the doorbell in memory, without restarting anything." \
     "          Takes under 2 seconds. Profile validation in Home Assistant needs it." \
     "  legacy  edits the doorbell configuration files and restarts its processes." \
     "          Takes about 30 seconds. Use it only if ipc does not work on your firmware."
while :; do
  apply_method="$(ask_field "Apply method (ipc/legacy)" ".unifi.apply_method // \"ipc\"" UNIFI_APPLY_METHOD)"
  case "$apply_method" in
    ipc|IPC|legacy) break ;;
    *) err "Expected ipc or legacy" ;;
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

printf '\n%sSaved %s%s\n' "$TXT_OK" "$CONFIG" "$TXT_RESET"

if [ "$(cat /proc/1/comm 2>/dev/null)" = "doorbell-mqtt-u" ]; then
  printf '%sThe service is already running: restart the container to apply (docker restart <container>).%s\n' "$TXT_DIM" "$TXT_RESET"
else
  echo "The service will start automatically."
fi