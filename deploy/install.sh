#!/bin/sh
# Install lampbus as a service for the current user. Review it before running; it changes nothing outside your home.
#   ./install.sh [path/to/lampbus]      default: ./lampbus, else ../target/release/lampbus
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
BIN=${1:-}
[ -n "$BIN" ] || for c in "$HERE/lampbus" "$HERE/../target/release/lampbus"; do [ -x "$c" ] && BIN=$c && break; done
[ -x "${BIN:-}" ] || { echo "no lampbus binary found; build with: cargo build --release -p lampbus" >&2; exit 1; }
mkdir -p "$HOME/.local/bin" "$HOME/.config/lampbus"
install -m 0755 "$BIN" "$HOME/.local/bin/lampbus"
echo "installed $HOME/.local/bin/lampbus ($("$HOME/.local/bin/lampbus" --version))"
case "$(uname -s)" in
  Linux)
    mkdir -p "$HOME/.config/systemd/user"
    cp "$HERE/lampbus.service" "$HOME/.config/systemd/user/lampbus.service"
    systemctl --user daemon-reload
    systemctl --user enable --now lampbus
    command -v loginctl >/dev/null && loginctl enable-linger "$USER" || true
    echo "running: systemctl --user status lampbus" ;;
  Darwin)
    mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
    sed "s|__HOME__|$HOME|g" "$HERE/com.lampbus.serve.plist" > "$HOME/Library/LaunchAgents/com.lampbus.serve.plist"
    launchctl bootout "gui/$(id -u)/com.lampbus.serve" 2>/dev/null || true
    launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.lampbus.serve.plist"
    echo "running: tail -f $HOME/Library/Logs/lampbus.log" ;;
  *) echo "unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac
echo "API token: $HOME/.config/lampbus/token   (created on first start)"
