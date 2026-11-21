#!/usr/bin/env bash
set -euo pipefail

BIN="${1:-./sonic-bridge-server}"
INSTALL_DIR="/usr/local/bin"
SERVICE="/etc/systemd/system/sonic-bridge.service"
CONFIG_DIR="/etc/sonic-bridge"
ENV_FILE="$CONFIG_DIR/sonic-bridge.env"

LISTEN="${SONIC_BRIDGE_LISTEN:-127.0.0.1:8765}"
FORWARD_TO="${SONIC_BRIDGE_FORWARD_TO:-}"
RELAY_ONLY="${SONIC_BRIDGE_RELAY_ONLY:-false}"
ALLOW_INSECURE="${SONIC_BRIDGE_ALLOW_INSECURE_LISTEN:-false}"
ALLOWED_PEERS="${SONIC_BRIDGE_ALLOWED_PEERS:-}"
KEY="${SONIC_BRIDGE_KEY:-}"

valid_endpoint() {
    [[ "$1" =~ ^[A-Za-z0-9.\[\]:_-]+:[0-9]{1,5}$ ]]
}

if [[ ! -x "$BIN" ]]; then
    echo "Binary not found or not executable: $BIN" >&2
    exit 1
fi

if ! valid_endpoint "$LISTEN"; then
    echo "Invalid SONIC_BRIDGE_LISTEN value: $LISTEN" >&2
    exit 1
fi

if [[ -n "$FORWARD_TO" ]] && ! valid_endpoint "$FORWARD_TO"; then
    echo "Invalid SONIC_BRIDGE_FORWARD_TO value: $FORWARD_TO" >&2
    exit 1
fi

if [[ -n "$KEY" ]] && [[ ! "$KEY" =~ ^[0-9a-fA-F]{64}$ ]]; then
    echo "SONIC_BRIDGE_KEY must be exactly 64 hexadecimal characters." >&2
    exit 1
fi

if [[ -n "$ALLOWED_PEERS" ]] && [[ ! "$ALLOWED_PEERS" =~ ^[0-9a-fA-F:.,]+$ ]]; then
    echo "SONIC_BRIDGE_ALLOWED_PEERS must be a comma-separated list of IP addresses." >&2
    exit 1
fi

if [[ "$ALLOW_INSECURE" != "true" && "$ALLOW_INSECURE" != "false" ]]; then
    echo "SONIC_BRIDGE_ALLOW_INSECURE_LISTEN must be true or false." >&2
    exit 1
fi

if [[ "$RELAY_ONLY" != "true" && "$RELAY_ONLY" != "false" ]]; then
    echo "SONIC_BRIDGE_RELAY_ONLY must be true or false." >&2
    exit 1
fi

if [[ -z "$KEY" && -z "$ALLOWED_PEERS" && "$ALLOW_INSECURE" != "true" && "$LISTEN" != 127.0.0.1:* ]]; then
    echo "An exposed listener needs a key, an allowed-peer list, or an explicit insecure opt-in." >&2
    exit 1
fi

# The worker only needs audio-device access, not root.
if ! id sonicbridge >/dev/null 2>&1; then
    sudo useradd --system --no-create-home --home-dir /nonexistent \
        --shell /usr/sbin/nologin --user-group sonicbridge
fi
if ! getent group audio >/dev/null; then
    echo "The audio group is missing. Install the system audio stack first." >&2
    exit 1
fi
sudo usermod -a -G audio sonicbridge

sudo install -Dm755 "$BIN" "$INSTALL_DIR/sonic-bridge-server"
sudo install -d -m 0750 -o root -g root "$CONFIG_DIR"
sudo install -m 0600 /dev/null "$ENV_FILE"

{
    printf 'SONIC_BRIDGE_LISTEN=%s\n' "$LISTEN"
    printf 'SONIC_BRIDGE_RELAY_ONLY=%s\n' "$RELAY_ONLY"
    printf 'SONIC_BRIDGE_ALLOW_INSECURE_LISTEN=%s\n' "$ALLOW_INSECURE"
    [[ -n "$KEY" ]] && printf 'SONIC_BRIDGE_KEY=%s\n' "$KEY"
    [[ -n "$FORWARD_TO" ]] && printf 'SONIC_BRIDGE_FORWARD_TO=%s\n' "$FORWARD_TO"
    [[ -n "$ALLOWED_PEERS" ]] && printf 'SONIC_BRIDGE_ALLOWED_PEERS=%s\n' "$ALLOWED_PEERS"
} | sudo tee "$ENV_FILE" >/dev/null
sudo chmod 0600 "$ENV_FILE"
sudo chown root:root "$ENV_FILE"

sudo tee "$SERVICE" >/dev/null <<UNIT
[Unit]
Description=SonicBridge audio receiver
After=network-online.target sound.target
Wants=network-online.target

[Service]
Type=simple
User=sonicbridge
Group=sonicbridge
SupplementaryGroups=audio
EnvironmentFile=$ENV_FILE
ExecStart=/usr/local/bin/sonic-bridge-server
Restart=on-failure
RestartSec=2
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictSUIDSGID=true
LockPersonality=true
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6

[Install]
WantedBy=multi-user.target
UNIT

sudo chmod 0644 "$SERVICE"
sudo systemctl daemon-reload
sudo systemctl enable --now sonic-bridge.service

echo
echo "SonicBridge is running as the unprivileged sonicbridge user."
echo "Configuration: $ENV_FILE (root-readable only)"
sudo systemctl --no-pager --full status sonic-bridge.service
