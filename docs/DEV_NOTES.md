# Dev notes

Short, sharp lessons from working on this repo. Intended for humans and
future automated agents who need to skip the same potholes we already hit.

## Local toolchain

- All 6 CI gates can be mirrored locally on macOS (Apple Silicon). Rough
  setup, one time:
  ```
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # rust
  brew install gstreamer node@22 onnxruntime
  brew install --cask docker-desktop && open -a Docker
  ```
  After install, source `. "$HOME/.cargo/env"` per shell (zsh doesn't pick
  it up from `~/.zshrc` by default).

- `command -v rustup` returning empty does NOT mean rustup is missing —
  it usually means the current shell hasn't sourced `~/.cargo/env`. Check
  `[ -d ~/.rustup ] || ls ~/.cargo/bin/rustup` first.

- Per-change loop, in priority order. Run only the smallest one that
  exercises the file you touched:
  ```
  cargo fmt --all -- --check                          # ~1 s
  cargo check  --locked --workspace --all-targets     # ~3 s incremental
  cargo clippy --locked --workspace --all-targets -- -D warnings   # ~5 s
  cargo test   --locked --workspace --no-fail-fast    # ~4 s
  cargo test   -p nexus-types --features ts           # regenerates ui/src/api/types/
  cargo check  --locked -p nexus-pipeline --features gstreamer
  cargo check  --locked -p nexus-inference --features ort,ep-cpu \
       ORT_DYLIB_PATH=/opt/homebrew/lib/libonnxruntime.dylib
  cd ui && npm install && npm run typecheck && npm run build
  ```
  Burning a CI cycle for any of these is wasted iteration.

- ORT brew formula 1.25.x works at runtime against the `ort = "=2.0.0-rc.10"`
  crate via `load-dynamic`, even though CI pins 1.20.0 in the system-libs
  job. Set `ORT_DYLIB_PATH=/opt/homebrew/lib/libonnxruntime.dylib`.

## Cargo / Rust

- After removing or renaming a source file referenced from `Cargo.toml`'s
  `[[bin]]` / `[[test]]` tables, run `cargo check` on the touched crate
  before assuming the build is sane — stale build graphs occasionally
  resurrect deleted files.

- Never combine `#[derive(Default)]` AND `#[serde(default = "fn")]` on the
  same struct. The derive zeroes fields; serde's `default = "fn"` only fires
  for missing keys during deserialise. Operators reading a partial config
  and library code constructing `T::default()` then disagree on what
  "default" means. Hand-write `impl Default` whenever any field uses a
  custom serde default. Canonical example: `nexus-config::TrackerConfig`.

## CI behaviour

- `system-libs` job (gstreamer + ort) used to run for 10–20 min before
  caching landed (commit `312941f`). Apt mirror at Azure was slow. The
  cache now key-pins `pkg-config libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev`
  and the ORT tarball at `/opt/onnxruntime`. After cache warmup the job
  is ~30 s. If you bump the package list, also bump `version: N` on the
  `awalsh128/cache-apt-pkgs-action` step or you'll pull a stale cache.

- `cargo fmt --check` is required (not `continue-on-error`). A drift-free
  workspace is the contract.

- `ts-rs` exports per-type `.ts` files into `ui/src/api/types/`.
  `cargo test -p nexus-types --features ts` regenerates them; CI then
  runs `git diff --exit-code -- ui/src/api/types/`. Re-commit the
  generated files when you change a `#[derive(TS)]` type.

## Working with long-running processes

- Background-mode terminals get killed when the chat tool cleans up,
  taking child processes with them. For HTTP smoke tests against a
  long-running engine, prefer:
  ```
  ( engine & echo $! > pid ) && sleep 2 && curl … ; kill "$(cat pid)" ; wait
  ```
  in a single sync command instead of starting an async terminal.

## YOLO model + smoke test

The real ORT detector lives in
[`crates/nexus-inference/src/yolo.rs`](../crates/nexus-inference/src/yolo.rs)
and is gated by the `ort` cargo feature. The smoke test
`yolo_smoke_runs_on_synthetic_frame` only runs when both:

- the binary is built with `--features ort,ep-cpu` (so the `ort` symbols
  link), and
- the env var `NEXUS_TEST_YOLO_MODEL` points at an existing
  `yolo26n_dynamic.onnx` on disk.

The `models/` directory is in `.gitignore`. Stage it locally:

```bash
mkdir -p models
cp ../nexus-edge-ai-core/models/yolo26n_dynamic.onnx models/
cp ../nexus-edge-ai-core/models/models-manifest.json   models/
ORT_DYLIB_PATH=/opt/homebrew/lib/libonnxruntime.dylib \
NEXUS_TEST_YOLO_MODEL=$PWD/models/yolo26n_dynamic.onnx \
  cargo test --locked -p nexus-inference --features ort,ep-cpu \
  yolo_smoke -- --nocapture
```

The worker binary picks up the same model with
`NEXUS_WORKER_MODEL_KIND=yolo` + `NEXUS_WORKER_MODEL_PATH=$PWD/models/yolo26n_dynamic.onnx`.

## See also

- [`ARCHITECTURE.md`](ARCHITECTURE.md) — trait + pool + fail-soft pattern,
  L7 cache, frame-lifecycle spans, sampling.
- [`ROADMAP.md`](ROADMAP.md) — milestones M0 → M8.
- [`HARDWARE_TIERS.md`](HARDWARE_TIERS.md) — T10 / T24 / T36-S / T64 specs.
