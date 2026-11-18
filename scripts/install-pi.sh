#!/usr/bin/env bash
set -euo pipefail

BIN="\${1:-./sonic-bridge-server}"
INSTALL_DIR="/usr/local/bin"
SERVICE="/etc/systemd/system/sonic-bridge.service"

if [[ ! -x "$BIN" ]]; then
    echo "Binary not found or not executable: $BIN" >&2
    exit 1
fi

sudo install -Dm755 "$BIN" "$INSTALL_DIR/sonic-bridge-server"

sudo tee "$SERVICE" >/dev/null <<'UNIT'
[Unit]
Description=SonicBridge audio receiver
After=network-online.target pipewire.service bluetooth.service
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/sonic-bridge-server --listen 0.0.0.0:8765
Restart=always
RestartSec=2
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true

[Install]
WantedBy=multi-user.target
UNIT

sudo systemctl daemon-reload
sudo systemctl enable --now sonic-bridge.service

echo
echo "SonicBridge is now running."
sudo systemctl --no-pager --full status sonic-bridge.service
