#!/usr/bin/env bash
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"
APP_DIR="${APP_DIR:-$HOME/.local/share/applications}"
ICON_DIR="${ICON_DIR:-$HOME/.local/share/icons/hicolor/scalable/apps}"
QMAKE="${QMAKE:-$(command -v qmake6 || command -v qmake || true)}"
BUILD=1

case "${1:-}" in
    --no-build) BUILD=0 ;;
    -h|--help)
        echo "usage: ./install.sh [--no-build]"
        echo "  BIN_DIR, APP_DIR, ICON_DIR and QMAKE can be set in the environment."
        exit 0
        ;;
esac

if [ "$BUILD" -eq 1 ]; then
    if [ -z "$QMAKE" ]; then
        echo "qmake6 not found; set QMAKE=/path/to/qmake6" >&2
        exit 1
    fi
    (cd "$ROOT" && QMAKE="$QMAKE" cargo build --release)
fi

BIN="$ROOT/target/release/corcho"
if [ ! -x "$BIN" ]; then
    echo "$BIN is missing; build first or omit --no-build" >&2
    exit 1
fi

install -Dm755 "$BIN" "$BIN_DIR/corcho"
install -Dm644 "$ROOT/packaging/corcho.desktop" "$APP_DIR/corcho.desktop"
install -Dm644 "$ROOT/packaging/corcho.svg" "$ICON_DIR/corcho.svg"

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$APP_DIR" >/dev/null 2>&1 || true
fi
if command -v xdg-mime >/dev/null 2>&1; then
    xdg-mime default corcho.desktop x-scheme-handler/corcho >/dev/null 2>&1 || true
fi

echo "Installed corcho to $BIN_DIR/corcho"
