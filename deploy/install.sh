#!/bin/sh
# Install lampbus (and optionally the Matter adapter) as services for the current user.
# Changes nothing outside your home directory. Review it, or run with --dry-run first.
#
#   ./install.sh [--matter] [--dry-run] [--bin-dir DIR]
#
#   --matter      also install lampbus-matter (Apple Home / Google Home / Alexa / Home Assistant)
#   --dry-run     print what would happen and change nothing
#   --bin-dir DIR where the built binaries are (default: ./bin, else ../target/release)
set -eu

MATTER=0 DRY=0 BINDIR=""
while [ $# -gt 0 ]; do
  case "$1" in
    --matter) MATTER=1 ;;
    --dry-run) DRY=1 ;;
    --bin-dir) shift; BINDIR=${1:?--bin-dir needs a directory} ;;
    -h|--help) sed -n '2,10p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

HERE=$(cd "$(dirname "$0")" && pwd)
if [ -z "$BINDIR" ]; then
  for d in "$HERE/bin" "$HERE/../target/release"; do [ -x "$d/lampbus" ] && BINDIR=$d && break; done
fi
[ -x "${BINDIR:-}/lampbus" ] || { echo "no lampbus binary found; build with: cargo build --release -p lampbus -p lampbus-matter" >&2; exit 1; }
[ "$MATTER" = 0 ] || [ -x "$BINDIR/lampbus-matter" ] || { echo "no lampbus-matter binary in $BINDIR" >&2; exit 1; }

run() { if [ "$DRY" = 1 ]; then echo "+ $*"; else "$@"; fi; }

run mkdir -p "$HOME/.local/bin" "$HOME/.config/lampbus"
run install -m 0755 "$BINDIR/lampbus" "$HOME/.local/bin/lampbus"
[ "$MATTER" = 0 ] || run install -m 0755 "$BINDIR/lampbus-matter" "$HOME/.local/bin/lampbus-matter"

case "$(uname -s)" in
  Linux)
    run mkdir -p "$HOME/.config/systemd/user"
    run cp "$HERE/lampbus.service" "$HOME/.config/systemd/user/lampbus.service"
    [ "$MATTER" = 0 ] || run cp "$HERE/lampbus-matter.service" "$HOME/.config/systemd/user/lampbus-matter.service"
    run systemctl --user daemon-reload
    run systemctl --user enable --now lampbus
    [ "$MATTER" = 0 ] || run systemctl --user enable --now lampbus-matter
    # keep the services running after logout and start them at boot
    command -v loginctl >/dev/null 2>&1 && run loginctl enable-linger "$USER" || true
    SVCS=lampbus; [ "$MATTER" = 0 ] || SVCS="lampbus lampbus-matter"
    echo "status:  systemctl --user status $SVCS"
    echo "logs:    journalctl --user -u lampbus -f" ;;
  Darwin)
    run mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
    for name in lampbus:serve $( [ "$MATTER" = 0 ] || echo lampbus-matter:matter ); do
      plist="com.lampbus.${name#*:}.plist"
      if [ "$DRY" = 1 ]; then echo "+ install $plist into ~/Library/LaunchAgents (with __HOME__ replaced)"; else
        sed "s|__HOME__|$HOME|g" "$HERE/$plist" > "$HOME/Library/LaunchAgents/$plist"
        launchctl bootout "gui/$(id -u)/${plist%.plist}" 2>/dev/null || true
        launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/$plist"
      fi
    done
    echo "logs:    tail -f ~/Library/Logs/lampbus.log" ;;
  *) echo "unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac
echo "API token (created on first start): $HOME/.config/lampbus/token"
