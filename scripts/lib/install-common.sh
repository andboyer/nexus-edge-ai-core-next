#!/usr/bin/env bash
# Shared helpers for scripts/install.sh, scripts/uninstall.sh, and
# scripts/bootstrap.sh.  POSIX-ish bash (we rely on `set -o pipefail`
# and `[[ ]]`); intentionally no zsh-isms — install.sh has to run on a
# fresh Ubuntu Server box where `bash` is the only certain shell.
#
# This file is sourced, never executed.  Every function it defines is
# idempotent so install.sh can be re-run to upgrade in place.

set -euo pipefail

# --- Paths --------------------------------------------------------------------

# Customisable via env so test harnesses can install into a tmpdir.
NEXUS_PREFIX="${NEXUS_PREFIX:-/opt/nexus}"
NEXUS_CONFIG_DIR="${NEXUS_CONFIG_DIR:-/etc/nexus}"
NEXUS_STATE_DIR="${NEXUS_STATE_DIR:-/var/lib/nexus}"
NEXUS_SERVICE_USER="${NEXUS_SERVICE_USER:-nexus}"
NEXUS_SERVICE_GROUP="${NEXUS_SERVICE_GROUP:-nexus}"
NEXUS_SYSTEMD_DIR="${NEXUS_SYSTEMD_DIR:-/etc/systemd/system}"

# --- Logging ------------------------------------------------------------------

_color() { [[ -t 1 ]] && printf '\033[%sm' "$1" || true; }
_reset() { [[ -t 1 ]] && printf '\033[0m' || true; }

log()  { printf '%s[nexus]%s %s\n' "$(_color '1;36')" "$(_reset)" "$*"; }
warn() { printf '%s[nexus]%s %s\n' "$(_color '1;33')" "$(_reset)" "$*" >&2; }
err()  { printf '%s[nexus]%s %s\n' "$(_color '1;31')" "$(_reset)" "$*" >&2; }
die()  { err "$*"; exit 1; }

# --- Pre-flight ---------------------------------------------------------------

require_root() {
    if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
        die "must run as root (try: sudo $0 $*)"
    fi
}

require_cmd() {
    local cmd
    for cmd in "$@"; do
        command -v "$cmd" >/dev/null 2>&1 \
            || die "required command '$cmd' not found on PATH"
    done
}

# Linux x86_64 only for now.  Add an arm64 release in a follow-up.
require_linux_x86_64() {
    local kernel arch
    kernel="$(uname -s)"
    arch="$(uname -m)"
    [[ "$kernel" == "Linux"  ]] || die "only Linux is supported (saw: $kernel)"
    [[ "$arch"   == "x86_64" ]] || die "only x86_64 is supported (saw: $arch)"
}

# Best-effort Ubuntu detection.  Other glibc distros may work but we
# only ship-test on Ubuntu 24.04.
detect_distro_id() {
    if [[ -r /etc/os-release ]]; then
        # shellcheck disable=SC1091
        ( . /etc/os-release && printf '%s' "${ID:-unknown}" )
    else
        printf 'unknown'
    fi
}

# --- User + dirs --------------------------------------------------------------

ensure_user() {
    if ! id -u "$NEXUS_SERVICE_USER" >/dev/null 2>&1; then
        log "creating service user: $NEXUS_SERVICE_USER"
        useradd --system \
            --home-dir "$NEXUS_STATE_DIR" \
            --shell /usr/sbin/nologin \
            "$NEXUS_SERVICE_USER"
    fi
}

ensure_dirs() {
    install -d -o root                  -g root                  -m 0755 "$NEXUS_PREFIX"
    install -d -o root                  -g root                  -m 0755 "$NEXUS_PREFIX/releases"
    install -d -o root                  -g root                  -m 0755 "$NEXUS_CONFIG_DIR"
    install -d -o "$NEXUS_SERVICE_USER" -g "$NEXUS_SERVICE_GROUP" -m 0750 "$NEXUS_STATE_DIR"
    install -d -o "$NEXUS_SERVICE_USER" -g "$NEXUS_SERVICE_GROUP" -m 0750 "$NEXUS_STATE_DIR/state"
    install -d -o "$NEXUS_SERVICE_USER" -g "$NEXUS_SERVICE_GROUP" -m 0750 "$NEXUS_STATE_DIR/clips"
}

# --- Atomic-swap symlink ------------------------------------------------------

# Flip /opt/nexus/current -> releases/<version> with `ln -sfn` then a
# rename, both of which are atomic on POSIX.  The previous target (if
# any) is recorded into install-state.json as `previous_good_version`
# so the M-OTA updater (and `scripts/install.sh --rollback`) can
# revert without re-downloading anything.
swap_current_symlink() {
    local target_version="$1"
    local link="$NEXUS_PREFIX/current"
    local previous=""

    if [[ -L "$link" ]]; then
        previous="$(basename "$(readlink "$link")")"
    fi

    # ln -sfn is the canonical "atomic replace a symlink" recipe:
    # it creates a temp symlink with target_version then rename(2)s
    # it over the existing one.
    ln -sfn "releases/$target_version" "$link"

    if [[ -n "$previous" && "$previous" != "$target_version" ]]; then
        log "swapped current: $previous -> $target_version"
    else
        log "current -> $target_version"
    fi
    printf '%s' "$previous"
}

# --- Manifest verification ----------------------------------------------------

# Verify the tarball's accompanying .sha256 sidecar matches the
# tarball on disk.  Returns 0 on match, dies on mismatch.
verify_sha256() {
    local tarball="$1"
    local sha_file="$2"
    [[ -r "$tarball"  ]] || die "tarball not readable: $tarball"
    [[ -r "$sha_file" ]] || die "sha256 sidecar not readable: $sha_file"

    local expected actual
    # Sidecar format: "<sha256>  <filename>" (sha256sum's default).
    expected="$(awk '{print $1}' "$sha_file" | head -n1)"
    actual="$(sha256sum "$tarball" | awk '{print $1}')"

    [[ -n "$expected" ]] || die "sha256 sidecar empty: $sha_file"
    [[ "$expected" == "$actual" ]] \
        || die "sha256 mismatch:\n  expected $expected\n  actual   $actual"

    log "sha256 OK ($actual)"
}

# Walk every file listed in MANIFEST.json (written at build time by
# the release workflow) and verify each one's sha256.  Catches the
# case where someone hand-edited a file inside the extracted release
# dir between install and runtime.  Tolerates missing jq by falling
# back to a python one-liner.
verify_manifest_files() {
    local release_dir="$1"
    local manifest="$release_dir/MANIFEST.json"

    [[ -r "$manifest" ]] || die "release MANIFEST.json missing: $manifest"

    if command -v jq >/dev/null 2>&1; then
        jq -r '.files[] | "\(.sha256)  \(.path)"' "$manifest" \
            | ( cd "$release_dir" && sha256sum -c --quiet --strict - )
    else
        python3 - "$release_dir" "$manifest" <<'PY'
import hashlib, json, sys
release_dir, manifest_path = sys.argv[1], sys.argv[2]
with open(manifest_path) as f:
    manifest = json.load(f)
bad = []
for entry in manifest["files"]:
    p = f"{release_dir}/{entry['path']}"
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    if h.hexdigest() != entry["sha256"]:
        bad.append(entry["path"])
if bad:
    print("sha256 mismatch:", *bad, sep="\n  ", file=sys.stderr)
    sys.exit(1)
PY
    fi
    log "manifest sha256 OK ($(jq -r '.files | length' "$manifest" 2>/dev/null || echo '?') files)"
}

# --- install-state.json -------------------------------------------------------

# Single-row state file the M-OTA updater (and rollback) read.  Path
# is canonical so the updater can find it without env wiring.  Format
# matches the cloud's `update_assignment_current` row shape so the
# updater can mirror state without translation.
write_install_state() {
    local version="$1"
    local previous="${2:-}"
    local state_file="$NEXUS_CONFIG_DIR/install-state.json"

    install -d -o root -g root -m 0755 "$NEXUS_CONFIG_DIR"

    python3 - "$state_file" "$version" "$previous" <<'PY'
import json, os, sys, time
state_file, version, previous = sys.argv[1], sys.argv[2], sys.argv[3]

state = {}
if os.path.exists(state_file):
    try:
        with open(state_file) as f:
            state = json.load(f)
    except Exception:
        state = {}

# Promote the prior current to previous_good so a future install.sh
# --rollback (or the M-OTA updater) can flip back without
# re-downloading.
state["channel"]               = state.get("channel", "stable")
state["previous_good_version"] = previous or state.get("current_version")
state["current_version"]       = version
state["installed_at"]          = int(time.time())
# Track the systemd unit hash so the M-OTA updater can refuse to
# auto-update on top of operator hand-edits (see M_OTA.md
# "Compose-file tamper detection" — same idea, different file).
unit = "/etc/systemd/system/nexus-engine.service"
if os.path.exists(unit):
    import hashlib
    h = hashlib.sha256()
    with open(unit, "rb") as f:
        for c in iter(lambda: f.read(1 << 16), b""):
            h.update(c)
    state["systemd_unit_sha256"] = h.hexdigest()

with open(state_file + ".tmp", "w") as f:
    json.dump(state, f, indent=2, sort_keys=True)
    f.write("\n")
os.replace(state_file + ".tmp", state_file)
os.chmod(state_file, 0o644)
PY

    log "wrote $state_file"
}

# --- Tier config staging ------------------------------------------------------

# Copy etc-templates/tiers/<tier>.toml -> /etc/nexus/nexus.toml on
# FIRST install only.  Rewrites pack_path + ui_root to point at the
# atomic-swap symlink so upgrades that change either don't require
# editing nexus.toml.  Preserves operator edits on every subsequent
# install (the file lives in /etc, which is the contract).
stage_tier_config() {
    local tier="$1"
    local release_dir="$2"
    local target="$NEXUS_CONFIG_DIR/nexus.toml"
    local src="$release_dir/etc-templates/tiers/${tier}.toml"

    [[ -r "$src" ]] || die "tier template not found: $src"

    if [[ -e "$target" ]]; then
        log "preserving existing config: $target (tier template skipped)"
        return 0
    fi

    install -o root -g root -m 0644 "$src" "$target"
    # Tier templates use the Docker paths /usr/share/nexus/{models,ui}
    # because the same templates are bind-mounted into the container.
    # On bare-metal the equivalent lives under the atomic-swap root.
    sed -i \
        -e 's#/usr/share/nexus/models#/opt/nexus/current/share/models#g' \
        -e 's#/usr/share/nexus/ui#/opt/nexus/current/share/ui#g' \
        "$target"
    log "staged tier config: $tier -> $target"
}

# --- systemd unit -------------------------------------------------------------

install_systemd_unit() {
    local release_dir="$1"
    local src="$release_dir/etc-templates/systemd/nexus-engine.service"
    local target="$NEXUS_SYSTEMD_DIR/nexus-engine.service"

    [[ -r "$src" ]] || die "systemd unit template not found: $src"

    install -o root -g root -m 0644 "$src" "$target"
    log "installed systemd unit: $target"

    systemctl daemon-reload
}

# --- Health check -------------------------------------------------------------

# Poll the API until /api/health returns 200 or the timeout elapses.
# The engine takes a few seconds to run migrations + open the model
# pack on first boot; 60s is generous but still bounded.
wait_for_health() {
    local timeout="${1:-60}"
    local port="${2:-8089}"
    local i=0
    log "waiting for engine /api/health on :$port (up to ${timeout}s)..."
    while (( i < timeout )); do
        if curl -fsS -m 2 -o /dev/null "http://127.0.0.1:${port}/api/health" 2>/dev/null; then
            log "engine healthy after ${i}s"
            return 0
        fi
        sleep 1
        i=$((i + 1))
    done
    return 1
}
