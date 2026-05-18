# syntax=docker/dockerfile:1.7
# ---------------------------------------------------------------------------
# Single multi-stage Dockerfile for the Nexus engine.
#
#  stage 1: build the TypeScript UI with Node/Vite
#  stage 2: build the Rust binary (engine + probe) with GStreamer + ORT headers
#  stage 3: minimal runtime — debian-slim + GStreamer runtime + ORT shared libs
#
# Output layout in the runtime image:
#   /usr/local/bin/nexus-engine
#   /usr/local/bin/nexus-probe
#   /usr/share/nexus/ui/...      (served by tower_http::ServeDir)
#   /etc/nexus/nexus.toml        (mount your own config over this)
# ---------------------------------------------------------------------------

# ----------------------- stage 1: UI -------------------------------
FROM node:22-alpine AS ui-build
WORKDIR /work
COPY ui/package.json ui/package.json
RUN cd ui && npm install --no-fund --no-audit --silent
COPY ui ui
RUN cd ui && npm run build

# ----------------------- stage 2: Rust -----------------------------
FROM rust:1.82-slim-bookworm AS rust-build
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config build-essential cmake git ca-certificates curl \
    libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
    gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
    gstreamer1.0-libav \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Pin ORT version. M0 ships without ORT linkage; M1 enables `--features ep-cpu`
# in the cargo build line below. ORT shared libs are downloaded into /opt and
# discovered at runtime via load-dynamic + LD_LIBRARY_PATH.
ARG ORT_VERSION=1.20.0
RUN curl -fsSL https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}/onnxruntime-linux-x64-${ORT_VERSION}.tgz \
    | tar -xz -C /opt && mv /opt/onnxruntime-linux-x64-${ORT_VERSION} /opt/onnxruntime

# M-Install Checkpoint 2 — kitchen-sink EP build. The runtime image
# always ships the same set of binaries; which execution providers
# they're linked against is decided at build time via this ARG.
# The default is the empty set ("just CPU + stub ORT"), matching
# the M0/M1 dev cadence. The release pipeline overrides this to
# `ort,ep-cpu,ep-openvino,ep-cuda,ep-tensorrt` so a single image
# works on every tier from T10 (CPU only) through T64 (NVIDIA).
#
# The features are forwarded both to nexus-engine (for its own
# `gstreamer` flag) and to nexus-inference via cargo's --features
# resolution against workspace deps. ORT shared libs are pulled
# from /opt/onnxruntime at runtime regardless; only the cargo-side
# wiring changes between feature sets.
ARG FEATURES=

WORKDIR /work
# Copy manifests first for better layer caching.
COPY rust-toolchain.toml Cargo.toml ./
COPY crates/nexus-types/Cargo.toml      crates/nexus-types/
COPY crates/nexus-config/Cargo.toml     crates/nexus-config/
COPY crates/nexus-bus/Cargo.toml        crates/nexus-bus/
COPY crates/nexus-telemetry/Cargo.toml  crates/nexus-telemetry/
COPY crates/nexus-store/Cargo.toml      crates/nexus-store/
COPY crates/nexus-rules/Cargo.toml      crates/nexus-rules/
COPY crates/nexus-tracker/Cargo.toml    crates/nexus-tracker/
COPY crates/nexus-inference/Cargo.toml  crates/nexus-inference/
COPY crates/nexus-pipeline/Cargo.toml   crates/nexus-pipeline/
COPY crates/nexus-engine/Cargo.toml     crates/nexus-engine/
COPY crates/nexus-probe/Cargo.toml      crates/nexus-probe/

# Now the sources.
COPY crates crates

# Build the binaries. nexus-engine takes the FEATURES override
# (forwarded into nexus-inference for ORT + per-EP wiring); nexus-probe
# has no EP-relevant features so it builds with workspace defaults.
# Two separate `cargo build` calls because `--features` at workspace
# root requires `-p` or a single-package selection.
RUN cargo build --release -p nexus-engine --features "${FEATURES}" --bin nexus-engine \
    && cargo build --release -p nexus-probe --bin nexus-probe

# ----------------------- stage 3: runtime --------------------------
FROM debian:bookworm-slim AS runtime
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates tini \
    libgstreamer1.0-0 libgstreamer-plugins-base1.0-0 \
    gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
    gstreamer1.0-libav gstreamer1.0-tools \
    libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 1000 --create-home --shell /usr/sbin/nologin nexus

# ORT shared lib for load-dynamic (no-op until M1 enables ORT features).
COPY --from=rust-build /opt/onnxruntime /opt/onnxruntime
ENV LD_LIBRARY_PATH=/opt/onnxruntime/lib:${LD_LIBRARY_PATH}

# Engine + probe binaries.
COPY --from=rust-build /work/target/release/nexus-engine /usr/local/bin/nexus-engine
COPY --from=rust-build /work/target/release/nexus-probe  /usr/local/bin/nexus-probe

# UI assets — served at `/` by the engine.
COPY --from=ui-build /work/ui/dist /usr/share/nexus/ui

# Default model pack — baked into the image so a fresh `docker compose up`
# works without §6.4 host-side model staging. ~58 MB total (yolo26n ~10 MB,
# yolo_world ~48 MB, manifest ~8 KB). Mounted read-only via the image
# layer; the tier configs in `config/tiers/` point `pack_path` here.
# Operators with custom model packs override by setting
# `[inference.model].pack_path = "/var/lib/nexus/models"` in their
# nexus.toml and bind-mounting `/var/lib/nexus/models/` over the read-only
# image dir. Path mirrors `/usr/share/nexus/ui/` so neither is shadowed
# by the `/var/lib/nexus` state bind from the per-tier overlays.
RUN mkdir -p /usr/share/nexus/models
COPY models/yolo26n_dynamic.onnx     /usr/share/nexus/models/yolo26n_dynamic.onnx
COPY models/yolo_world_v2_s.onnx     /usr/share/nexus/models/yolo_world_v2_s.onnx
COPY models/models-manifest.json     /usr/share/nexus/models/models-manifest.json

# Default config + state dirs.
RUN mkdir -p /etc/nexus /var/lib/nexus && chown -R nexus:nexus /var/lib/nexus
COPY config/single-camera.toml /etc/nexus/nexus.toml

USER nexus
EXPOSE 8089
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/nexus-engine"]
CMD ["--config", "/etc/nexus/nexus.toml"]
