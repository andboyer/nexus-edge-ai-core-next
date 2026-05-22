# `tools/models/` — model generators (M3+)

Reproducible exports for every ONNX artifact the engine loads. The
artifacts themselves are gitignored (they're large binary blobs); the
scripts here are the source of truth and run inside the dedicated
`.venv-modelgen` virtualenv.

## Setup (one-time)

The model-gen toolchain is heavy (torch, ultralytics, transformers
sometimes). It lives in its own Python venv so it never collides with
the runtime Python (or with the OS Homebrew Python and PEP 668).

```bash
# Python 3.11 — torch + ultralytics ship full wheels for it; 3.14 is too new.
/opt/homebrew/opt/python@3.11/bin/python3.11 -m venv .venv-modelgen
source .venv-modelgen/bin/activate
pip install -r tools/models/requirements.txt
```

If you keep a single venv at the workspace root, that's fine — every
script in this directory is `cd`-independent and writes into the
repo's `models/` directory by absolute path.

## Generators

| Script | Output | Used by |
|---|---|---|
| `gen_yolo26n.py` | `models/yolo26n_dynamic.onnx` (~10 MB) | M1 closed-vocab detector (`YoloOrtDetector`). |
| `gen_yolo_world.py` | `models/yolo_world_v2_s.onnx` (~50–80 MB) | M3 open-vocab detector (`YoloWorldDetector`). Embeds the text encoder into the graph and bakes the operator-supplied prompt vocabulary as fixed text inputs. |
| `gen_yoloe.py` | `models/yoloe26_s.onnx` (~25–35 MB) | M3.1 text-mode YOLOE detector (`YoloeDetector`). Mirrors `gen_yolo_world.py` against the upstream `ultralytics.YOLOE` checkpoint. |
| `gen_yoloe_visual.py` | `models/yoloe26_s_image_encoder.onnx` (~15–20 MB) | M3.1 visual-prompt encoder for `YoloeVisualDetector`. Run AFTER `gen_yoloe.py`; produces the standalone image-embedding ONNX the engine's admin upload path uses to encode reference crops. |

Run them from the repo root with the venv active:

```bash
python tools/models/gen_yolo26n.py
python tools/models/gen_yolo_world.py --prompts models/yolo_world_default_prompts.txt
python tools/models/gen_yoloe.py --prompts tools/models/yoloe_default_prompts.txt
python tools/models/gen_yoloe_visual.py
```

Both scripts:

* are idempotent — re-running with the same args produces the same
  artifact (modulo any non-deterministic export choice in ultralytics
  itself, which we slim away with `onnxslim`),
* refresh `models/models-manifest.json` with the new sha256 so the
  engine's manifest loader (W-DETECT D5) catches a stale download,
* exit non-zero on any error so CI can wire them up later (NOT in M3
  scope — the artifacts stay gitignored and operator-built, but the
  exit-code contract is in place).

## Why a separate venv?

* The runtime engine doesn't ship Python.
* The model-gen deps (torch ~2 GB, ultralytics, onnxslim) are
  developer-only and would dominate any prod image.
* Pinning Python 3.11 is the only way to get torch CPU wheels on
  macOS Apple Silicon today; Homebrew defaults to Python 3.14 which
  has no torch wheels.
