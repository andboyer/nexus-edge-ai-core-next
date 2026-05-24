#!/usr/bin/env bash
# scripts/install.sh — bare-metal installer for the Nexus engine.
#
# Two invocation modes:
#
#   1. From inside an extracted release tarball (the usual case
#      driven by scripts/bootstrap.sh):
#
#         cd /opt/nexus/releases/v0.2.0
#         sudo ./scripts/install.sh --tier t24
#
#   2. From a clone of this repo, against a release tarball you
#      downloaded yourself:
#
#         sudo scripts/install.sh \
#             --tier t24 \
#             --tarball ~/Downloads/nexus-edge-v0.2.0-linux-x86_64.tar.gz
#
# Either way the script:
#
#   * verifies the tarball's .sha256 sidecar (mode 2 only),
#   * extracts it under /opt/nexus/releases/<version>/ if not already
#     present,
#   * verifies every file's sha256 against MANIFEST.json,
#   * creates the `nexus` system user + /etc/nexus + /var/lib/nexus
#     on first run,
#   * stages /etc/nexus/nexus.toml from the chosen tier template (only
#     on first run; preserves operator edits forever after),
#   * installs the systemd unit,
#   * atomically flips /opt/nexus/current to the new release,
#   * (re)starts nexus-engine.service and waits for /api/health.
#
# Idempotent: re-running with the same --tier on the same version
# is a no-op except for `systemctl restart`.  Re-running with a
# different --tier rewrites /etc/nexus/nexus.toml ONLY if you also
# pass --force-tier (because the operator may have hand-tuned).
#
# This file lives inside the tarball at scripts/install.sh and is
# also tracked in the repo at scripts/install.sh — the two are
# identical (release workflow copies the latter into the tarball).

set -euo pipefail

# Resolve our own directory so `source ./lib/install-common.sh` works
# whether the script was invoked from `/opt/nexus/releases/v.../` or
# from a checkout.
SCRIPT_DIR="$( cd "$(dirname "${BASH_SOURCE[0]}")" && pwd )"
# shellcheck source=lib/install-common.sh
. "$SCRIPT_DIR/lib/install-common.sh"

# --- Arg parsing --------------------------------------------------------------

TIER=""
TARBALL=""
VERSION=""
FORCE_TIER=0
NO_START=0
ROLLBACK=0
SKIP_SYSTEM_PREP=0
# Per-step prep flags. Default ON so a fresh install is a one-liner;
# operators with hardened base images can opt out individually.
export NEXUS_PREP_DEPS="${NEXUS_PREP_DEPS:-1}"
export NEXUS_PREP_SWAP="${NEXUS_PREP_SWAP:-1}"
export NEXUS_PREP_FIREWALL="${NEXUS_PREP_FIREWALL:-1}"
export NEXUS_PREP_AUTO_UPDATES="${NEXUS_PREP_AUTO_UPDATES:-0}"
export NEXUS_INSTALL_DRIVERS="${NEXUS_INSTALL_DRIVERS:-1}"

usage() {
    cat <<EOF
Usage: $0 [options]

Options:
  --tier {t10|t24|t36|t36s|t64}   Pick the tier config template (required on
                                  first install; ignored on upgrades unless
                                  --force-tier is also passed). Omit to let
                                  nexus-probe pick on first install.
  --tarball <path>                Install from a .tar.gz on disk (defaults
                                  to assuming we're already inside an
                                  extracted release).
  --version <vX.Y.Z>              Override the version string (defaults to
                                  the VERSION file inside the release).
  --force-tier                    Overwrite /etc/nexus/nexus.toml with the
                                  chosen tier template, even if a config
                                  already exists.  Backs up the old file
                                  to nexus.toml.bak first.
  --no-start                      Install everything but don't enable or
                                  start the systemd unit.
  --rollback                      Flip /opt/nexus/current to the
                                  previous_good_version recorded in
                                  install-state.json and restart.

Host-preparation flags (all ON by default — opt out per-step):
  --skip-system-prep              Skip ALL host prep (apt installs, swap,
                                  ufw rules). Use when running against
                                  a pre-hardened base image.
  --no-deps                       Skip apt-installing GStreamer runtime +
                                  chrony + ufw + jq.
  --no-swap                       Don't create /swapfile if no swap exists.
  --no-firewall                   Don't add ufw allow rules for 80 + 8089
                                  (only relevant when ufw is already active).
  --enable-auto-updates           Install + enable unattended-upgrades for
                                  security patches (off by default; auto-
                                  reboots are disabled either way).
  --no-drivers                    Skip accelerator driver auto-install.
                                  By default install.sh lspci-probes the
                                  box and installs the Intel iGPU / Arc
                                  dGPU / NPU drivers it finds. If the
                                  NPU needs an HWE kernel upgrade,
                                  install.sh stages the kernel and
                                  exits asking for a reboot.

  -h, --help                      This message.

Environment:
  NEXUS_PREFIX     install root (default /opt/nexus)
  NEXUS_CONFIG_DIR config dir    (default /etc/nexus)
  NEXUS_STATE_DIR  state dir     (default /var/lib/nexus)
  NEXUS_PREP_*     per-step prep toggles (same as flags above, env form)
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --tier)               TIER="$2"; shift 2 ;;
        --tarball)            TARBALL="$2"; shift 2 ;;
        --version)            VERSION="$2"; shift 2 ;;
        --force-tier)         FORCE_TIER=1; shift ;;
        --no-start)           NO_START=1; shift ;;
        --rollback)           ROLLBACK=1; shift ;;
        --skip-system-prep)   SKIP_SYSTEM_PREP=1; shift ;;
        --no-deps)            export NEXUS_PREP_DEPS=0; shift ;;
        --no-swap)            export NEXUS_PREP_SWAP=0; shift ;;
        --no-firewall)        export NEXUS_PREP_FIREWALL=0; shift ;;
        --enable-auto-updates) export NEXUS_PREP_AUTO_UPDATES=1; shift ;;
        --no-drivers)         export NEXUS_INSTALL_DRIVERS=0; shift ;;
        -h|--help)            usage; exit 0 ;;
        *)                    err "unknown option: $1"; usage; exit 2 ;;
    esac
done

# --- Pre-flight ---------------------------------------------------------------

require_root "$@"
require_linux_x86_64
require_cmd curl tar sha256sum systemctl install useradd python3

distro_id="$(detect_distro_id)"
if [[ "$distro_id" != "ubuntu" && "$distro_id" != "debian" ]]; then
    warn "distro '$distro_id' is not Ubuntu/Debian — proceeding anyway, YMMV"
fi

# --- Rollback path (no extract / no manifest verify needed) -------------------

if (( ROLLBACK )); then
    state_file="$NEXUS_CONFIG_DIR/install-state.json"
    [[ -r "$state_file" ]] || die "no install-state.json — nothing to roll back to"
    previous="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("previous_good_version") or "")' "$state_file")"
    [[ -n "$previous" ]] || die "previous_good_version unset in $state_file — nothing to roll back to"
    [[ -d "$NEXUS_PREFIX/releases/$previous" ]] \
        || die "previous_good release dir missing: $NEXUS_PREFIX/releases/$previous"

    log "rolling back to $previous"
    current="$(swap_current_symlink "$previous")"
    # Swap previous_good <-> current_version in the state file so a
    # second --rollback is reversible.
    write_install_state "$previous" "$current"
    systemctl restart nexus-engine
    wait_for_health 60 || die "engine did not become healthy after rollback"
    log "rollback complete: now running $previous"
    exit 0
fi

# --- Locate or extract the release directory ----------------------------------

# Cases:
#   (a) --tarball given: extract to NEXUS_PREFIX/releases/<VERSION>/
#   (b) --tarball not given but we're inside an extracted release
#       (VERSION file present alongside scripts/): use SCRIPT_DIR/..
#   (c) --tarball not given but we're inside a checkout (no VERSION
#       file): error — operator should download a tarball.

RELEASE_DIR=""

if [[ -n "$TARBALL" ]]; then
    [[ -r "$TARBALL" ]] || die "tarball not readable: $TARBALL"
    sha_file="${TARBALL}.sha256"
    [[ -r "$sha_file" ]] || die "expected sha256 sidecar next to tarball: $sha_file"
    verify_sha256 "$TARBALL" "$sha_file"

    # Pull VERSION out of the tarball without doing a full extract.
    extracted_version="$(tar -xzOf "$TARBALL" --wildcards '*/VERSION' 2>/dev/null | head -n1)"
    [[ -n "$extracted_version" ]] || die "tarball is missing VERSION file"
    VERSION="${VERSION:-$extracted_version}"

    RELEASE_DIR="$NEXUS_PREFIX/releases/$VERSION"
    if [[ -d "$RELEASE_DIR" ]]; then
        log "release dir already exists: $RELEASE_DIR (re-using, skipping extract)"
    else
        install -d -o root -g root -m 0755 "$NEXUS_PREFIX/releases"
        log "extracting $TARBALL -> $RELEASE_DIR"
        tmpdir="$(mktemp -d -p "$NEXUS_PREFIX/releases" .extract.XXXXXX)"
        # Strip the top-level dir baked into the tarball so we land
        # directly at $RELEASE_DIR/{bin,lib,share,...}.
        tar -xzf "$TARBALL" -C "$tmpdir" --strip-components=1
        mv "$tmpdir" "$RELEASE_DIR"
        chown -R root:root "$RELEASE_DIR"
    fi
else
    if [[ -r "$SCRIPT_DIR/../VERSION" ]]; then
        RELEASE_DIR="$( cd "$SCRIPT_DIR/.." && pwd )"
        VERSION="${VERSION:-$(cat "$RELEASE_DIR/VERSION")}"
        log "installing from extracted release: $RELEASE_DIR (version $VERSION)"
    else
        die "no --tarball given and no VERSION file alongside scripts/ — \
either run from inside an extracted release or pass --tarball <path>"
    fi
fi

[[ -d "$RELEASE_DIR/bin" ]]            || die "release $RELEASE_DIR missing bin/"
[[ -d "$RELEASE_DIR/etc-templates" ]]  || die "release $RELEASE_DIR missing etc-templates/"
[[ -d "$RELEASE_DIR/share" ]]          || die "release $RELEASE_DIR missing share/"

# --- Verify every file in the release ----------------------------------------

verify_manifest_files "$RELEASE_DIR"

# --- Verify the operator-key Ed25519 signature -------------------------------
#
# Optional today (loud warning if absent — see verify_signature()
# notes in install-common.sh) so the first-cut release tarball that
# ships before the GH signing secret is onboarded still installs.
# Set NEXUS_REQUIRE_SIGNATURE=1 to flip this to strict.

verify_signature "$RELEASE_DIR"

# --- Host prep (idempotent) ---------------------------------------------------

if (( SKIP_SYSTEM_PREP )); then
    log "--skip-system-prep: leaving apt prereqs / swap / ufw rules alone"
else
    system_prep
fi

# --- Accelerator drivers (idempotent; lspci-probed) ---------------------------
# Runs BEFORE ensure_user so a Lunar Lake box that needs an HWE
# kernel reboot exits cleanly without staging the engine half-way.
# Honours --no-drivers / NEXUS_INSTALL_DRIVERS=0.

install_drivers

# --- User + dirs --------------------------------------------------------------

ensure_user
ensure_dirs
ensure_accelerator_groups

# --- Stage tier config (first install only, or --force-tier) ------------------

if [[ -n "$TIER" ]]; then
    case "$TIER" in t10|t24|t36|t36s|t64) ;; *) die "unknown --tier: $TIER" ;; esac

    if (( FORCE_TIER )); then
        if [[ -e "$NEXUS_CONFIG_DIR/nexus.toml" ]]; then
            backup="$NEXUS_CONFIG_DIR/nexus.toml.bak.$(date +%Y%m%dT%H%M%S)"
            log "--force-tier: backing up existing config to $backup"
            cp -a "$NEXUS_CONFIG_DIR/nexus.toml" "$backup"
            rm -f "$NEXUS_CONFIG_DIR/nexus.toml"
        fi
    fi
    stage_tier_config "$TIER" "$RELEASE_DIR"
elif [[ ! -e "$NEXUS_CONFIG_DIR/nexus.toml" ]]; then
    # First install + no explicit --tier: ask nexus-probe what this
    # box looks like and use its `recommended_tier`. Falls back to
    # the original "please pass --tier" error if probe doesn't
    # produce a usable answer (no GPU on a recent CPU might still
    # land on the right tier; an ancient box without AVX2 might
    # not).
    auto_tier="$(auto_detect_tier "$RELEASE_DIR")"
    if [[ -n "$auto_tier" ]]; then
        log "nexus-probe recommends --tier $auto_tier; using it (override with --tier on re-run)"
        TIER="$auto_tier"
        stage_tier_config "$TIER" "$RELEASE_DIR"
    else
        die "no /etc/nexus/nexus.toml, no --tier given, and nexus-probe could not auto-detect — pass --tier t{10,24,36,36s,64}"
    fi
else
    log "preserving existing config: $NEXUS_CONFIG_DIR/nexus.toml"
fi

# --- systemd unit -------------------------------------------------------------

install_systemd_unit "$RELEASE_DIR"

# --- Atomic swap --------------------------------------------------------------

previous="$(swap_current_symlink "$VERSION")"
write_install_state "$VERSION" "$previous"

# --- Start the service --------------------------------------------------------

if (( NO_START )); then
    log "--no-start given; leaving systemd unit disabled"
    log "to start later: sudo systemctl enable --now nexus-engine"
    exit 0
fi

if systemctl is-active --quiet nexus-engine; then
    log "restarting nexus-engine.service"
    systemctl restart nexus-engine
else
    log "enabling + starting nexus-engine.service"
    systemctl enable --now nexus-engine
fi

if ! wait_for_health 60; then
    err "engine did not become healthy within 60s"
    err "recent logs:"
    journalctl -u nexus-engine -n 40 --no-pager >&2 || true
    err "to roll back: sudo $0 --rollback"
    exit 1
fi

log ""
log "================================================================"
log "  nexus-engine $VERSION installed and healthy."
log "  UI:    http://$(hostname -f 2>/dev/null || hostname)/"
log "  API:   http://$(hostname -f 2>/dev/null || hostname):8089/api/health"
log ""

# --- First-boot admin credentials --------------------------------------------
# The engine writes a one-time bootstrap sentinel file when the admin
# account is first created. We poll for it for up to 30 seconds (the
# write happens just after the engine reaches healthy, but we already
# waited 60s above so it should already exist for true first boots).
# If the latch is absent the engine already has a permanent password
# and we don't print anything sensitive.
SENTINEL="${NEXUS_STATE_DIR}/bootstrap-password.txt"
sentinel_deadline=$(( $(date +%s) + 30 ))
while [[ ! -f "$SENTINEL" && $(date +%s) -lt $sentinel_deadline ]]; do
    sleep 1
done

if [[ -f "$SENTINEL" ]]; then
    bootstrap_user="$(awk -F'\t' 'NR==1{print $1}' "$SENTINEL" 2>/dev/null || echo admin)"
    bootstrap_pw="$(awk -F'\t' 'NR==1{print $2}' "$SENTINEL" 2>/dev/null || echo '')"
    log "  First-boot admin credentials (printed ONCE):"
    log "    user:     $bootstrap_user"
    log "    password: $bootstrap_pw"
    log ""
    log "  The setup wizard will guide you through changing this"
    log "  password and adding your first cameras and rules. After"
    log "  the password change the engine deletes the file at:"
    log "    $SENTINEL"
else
    log "  Admin password already set (no bootstrap file present)."
    log "  To recover access from a forgotten password, run:"
    log "    sudo -u $NEXUS_SERVICE_USER /opt/nexus/current/bin/nexus-doctor reset-admin"
fi

log ""
log "  Smoke-test:"
log "    sudo -u $NEXUS_SERVICE_USER /opt/nexus/current/bin/nexus-doctor"
log "================================================================"
