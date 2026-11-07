#!/bin/sh
# behe-vty installer.
# Builds the project and installs the binary as three commands:
#   behe-vty, behe-terminal, behe-console
#
# Usage:
#   ./install.sh              install system-wide into /usr/local/bin (uses sudo)
#   ./install.sh --user       install into ~/.local/bin (no root needed)

set -eu

BANNER='██████╗ ███████╗██╗  ██╗███████╗    ██╗   ██╗████████╗██╗   ██╗
██╔══██╗██╔════╝██║  ██║██╔════╝    ██║   ██║╚══██╔══╝╚██╗ ██╔╝
██████╔╝█████╗  ███████║█████╗█████╗██║   ██║   ██║    ╚████╔╝ 
██╔══██╗██╔══╝  ██╔══██║██╔══╝╚════╝╚██╗ ██╔╝   ██║     ╚██╔╝  
██████╔╝███████╗██║  ██║███████╗     ╚████╔╝    ██║      ██║   
╚═════╝ ╚══════╝╚═╝  ╚═╝╚══════╝      ╚═══╝     ╚═╝      ╚═╝  '
printf '%s\n' "$BANNER"

BIN_NAME="behe-vty"
ALIASES="behe-terminal behe-console"

USER_INSTALL=0
if [ "${1:-}" = "--user" ]; then
    USER_INSTALL=1
elif [ $# -gt 0 ]; then
    echo "Usage: $0 [--user]" >&2
    exit 1
fi

log() {
    printf '==> %s\n' "$1"
}

die() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

if ! command -v cargo >/dev/null 2>&1; then
    die "cargo not found. Install the Rust toolchain first: https://rustup.rs"
fi

log "Building release binary..."
cargo build --release

BINARY="target/release/${BIN_NAME}"
[ -f "$BINARY" ] || die "build finished but ${BINARY} was not found"

SUDO=""
if [ "$USER_INSTALL" -eq 1 ]; then
    DEST_DIR="${HOME}/.local/bin"
else
    DEST_DIR="/usr/local/bin"
    if [ "$(id -u)" -ne 0 ]; then
        command -v sudo >/dev/null 2>&1 || die "sudo not available; rerun with --user to install into ~/.local/bin"
        SUDO="sudo"
    fi
fi

log "Installing to ${DEST_DIR} ..."
mkdir -p "$DEST_DIR" 2>/dev/null || true
$SUDO install -m 755 "$BINARY" "${DEST_DIR}/${BIN_NAME}"

log "Creating command aliases: ${ALIASES}"
for alias_name in $ALIASES; do
    $SUDO rm -f "${DEST_DIR}/${alias_name}"
    $SUDO ln -s "${DEST_DIR}/${BIN_NAME}" "${DEST_DIR}/${alias_name}"
done

case ":${PATH}:" in
    *":${DEST_DIR}":*) ;;
    *)
        printf 'warning: %s is not on your PATH.\n' "$DEST_DIR"
        printf 'Add it with: export PATH="%s:$PATH"\n' "$DEST_DIR"
        ;;
esac

log "Done. Available commands: behe-vty, behe-terminal, behe-console"

