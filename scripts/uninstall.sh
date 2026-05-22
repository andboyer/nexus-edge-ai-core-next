#!/usr/bin/env bash
# scripts/uninstall.sh — symmetric removal of a bare-metal install
# done by scripts/install.sh.
#
# Default behaviour (safe):
#   * stops + disables the systemd unit
#   * removes the unit file + the /opt/nexus tree
#   * leaves /etc/nexus/, /var/lib/nexus/, and the `nexus` user alone
#     so a re-install picks up where you left off
#
# Pass --purge to also wipe config + state + the service user.  That
# nukes the SQLite db, all recorded clips, every operator-tuned config
# value, and the admin secret.  There is no undo.

set -euo pipefail

SCRIPT_DIR="$( cd "$(dirname "${BASH_SOURCE[0]}")" && pwd )"
# shellcheck source=lib/install-common.sh
. "$SCRIPT_DIR/lib/install-common.sh"

PURGE=0
KEEP_RELEASES=0

usage() {
    cat <<EOF
Usage: $0 [options]

Options:
  --purge          Also remove /etc/nexus, /var/lib/nexus, and the
                   '$NEXUS_SERVICE_USER' user.  DESTROYS the SQLite
                   db, all recorded clips, and operator config.
  --keep-releases  Don't remove $NEXUS_PREFIX/releases/* (default is
                   to remove the whole $NEXUS_PREFIX tree).
  -h, --help       This message.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --purge)         PURGE=1; shift ;;
        --keep-releases) KEEP_RELEASES=1; shift ;;
        -h|--help)       usage; exit 0 ;;
        *)               err "unknown option: $1"; usage; exit 2 ;;
    esac
done

require_root "$@"

unit="$NEXUS_SYSTEMD_DIR/nexus-engine.service"
if systemctl list-unit-files nexus-engine.service >/dev/null 2>&1; then
    if systemctl is-active --quiet nexus-engine; then
        log "stopping nexus-engine.service"
        systemctl stop nexus-engine
    fi
    if systemctl is-enabled --quiet nexus-engine 2>/dev/null; then
        log "disabling nexus-engine.service"
        systemctl disable nexus-engine
    fi
fi
if [[ -e "$unit" ]]; then
    log "removing $unit"
    rm -f "$unit"
    systemctl daemon-reload
fi

if (( KEEP_RELEASES )); then
    if [[ -L "$NEXUS_PREFIX/current" ]]; then
        log "removing $NEXUS_PREFIX/current symlink (keeping releases/)"
        rm -f "$NEXUS_PREFIX/current"
    fi
else
    if [[ -d "$NEXUS_PREFIX" ]]; then
        log "removing $NEXUS_PREFIX"
        rm -rf "$NEXUS_PREFIX"
    fi
fi

if (( PURGE )); then
    if [[ -d "$NEXUS_CONFIG_DIR" ]]; then
        warn "purging $NEXUS_CONFIG_DIR (operator config, install-state)"
        rm -rf "$NEXUS_CONFIG_DIR"
    fi
    if [[ -d "$NEXUS_STATE_DIR" ]]; then
        warn "purging $NEXUS_STATE_DIR (db, clips, admin secret)"
        rm -rf "$NEXUS_STATE_DIR"
    fi
    if id -u "$NEXUS_SERVICE_USER" >/dev/null 2>&1; then
        warn "removing service user: $NEXUS_SERVICE_USER"
        userdel "$NEXUS_SERVICE_USER" || true
    fi
fi

log "uninstall complete"
