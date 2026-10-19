#!/bin/sh
set -e

if [ "$(id -u)" -ne 0 ]; then
    echo "Error: This script must be run with sudo to install system-wide session files."
    echo "Please run: sudo $0"
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "Installing purpwm to /usr/local/bin/purpwm..."
install -m 755 "$SCRIPT_DIR/target/release/purpwm" /usr/local/bin/purpwm

echo "Installing start-purpwm session launcher to /usr/local/bin/start-purpwm..."
install -m 755 "$SCRIPT_DIR/start-purpwm" /usr/local/bin/start-purpwm

echo "Installing purpwm-wifi, purpwm-bt, and purpwm-polkit-agent to /usr/local/bin/..."
install -m 755 "$SCRIPT_DIR/purpwm-wifi" /usr/local/bin/purpwm-wifi
install -m 755 "$SCRIPT_DIR/purpwm-bt" /usr/local/bin/purpwm-bt
install -m 755 "$SCRIPT_DIR/purpwm-polkit-agent" /usr/local/bin/purpwm-polkit-agent

echo "Installing Wayland session entry to /usr/share/wayland-sessions/purpwm.desktop..."
install -m 644 "$SCRIPT_DIR/purpwm.desktop" /usr/share/wayland-sessions/purpwm.desktop

echo ""
echo "Successfully installed purpwm (with 2560x1600 native resolution, cursor, Wi-Fi, Bluetooth & OS Keyring/Polkit) to COSMIC greeter!"
