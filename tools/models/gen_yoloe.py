#!/usr/bin/env python3
"""
Generate the text-mode YOLOE ONNX used by M3's `YoloeDetector`.

YOLOE is the open-vocabulary detector line that supersedes YOLO-World
in the ultralytics ecosystem. This script is the M3.1 counterpart of
`gen_yolo_world.py`: same vocab-baking flow, different upstream
checkpoint. The Rust detector (`crates/nexus-inference/src/yoloe.rs`)
treats the resulting ONNX as a closed-vocab YOLOv8-style head where
each class index 0..N-1 maps to a prompt in `prompts[]`.

Run from the workspace root with the model-gen venv active:

    source .venv-modelgen/bin/activate
    python tools/models/gen_yoloe.py \\
        --prompts tools/models/yoloe_default_prompts.txt

Output:
    models/yoloe26_s.onnx   (~25–35 MB; smaller than YOLO-World v2 s)

The prompt file lives under `tools/models/` (tracked) so the prompt
vocabulary is reproducible; the ONNX itself stays under `models/`
(gitignored) per the same policy as `yolo26n_{640,960,1280}.onnx`.

NOTE on upstream availability: as of M3.1 the ultralytics PyPI release
shipping the `YOLOE` symbol is moving rapidly. If `from ultralytics
import YOLOE` fails, install from the main branch:

    pip install -U "git+https://github.com/ultralytics/ultralytics@main"

The export incantation (`model.set_classes(...)` then `model.export(
format="onnx", ...)`) mirrors `gen_yolo_world.py` exactly — the
ultralytics team kept the public surface stable across the YOLOE
rename.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import sys
from pathlib import Path
from typing import List

REPO_ROOT = Path(__file__).resolve().parents[2]
MODELS_DIR = REPO_ROOT / "models"
DEFAULT_OUTPUT = MODELS_DIR / "yoloe26_s.onnx"
DEFAULT_PROMPTS = Path(__file__).resolve().parent / "yoloe_default_prompts.txt"
# Ultralytics 8.4.x consolidated the YOLOE release assets on the segmentation
# checkpoints (`yoloe-26{n,s,l,x}-seg.pt` and `yoloe-v8{s,m,l}-seg.pt`); the
# bare `yoloe-s.pt` originally referenced here no longer exists upstream.
# We pick the 26-arch small variant (smallest with the new backbone) and
# ultralytics auto-strips the segmentation head when we export with the
# detection task. The Rust loader only consumes the detection output anyway.
DEFAULT_BASE_MODEL = "yoloe-26s-seg.pt"


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def read_prompts(path: Path) -> List[str]:
    """Read a one-prompt-per-line text file, skipping blanks + `#` comments."""

    out: List[str] = []
    seen: set[str] = set()
    for raw in path.read_text().splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line in seen:
            continue
        seen.add(line)
        out.append(line)
    if not out:
        raise ValueError(f"prompt file {path} produced no usable entries")
    return out


def upsert_manifest_entry(
    *,
    model_id: str,
    artifact_path: str,
    sha: str,
    input_w: int,
    input_h: int,
    prompts: List[str],
) -> None:
    """Insert/update a YOLOE entry in `models/models-manifest.json`.

    Re-uses the v2 manifest extension `prompts[]` block that
    YOLO-World introduced — the Rust loader keys off `task` to pick
    the detector backend, and YOLOE shares the open-vocab task tag
    `detect_open_vocab_text` (alias `detect_open_vocab` for legacy
    YOLO-World configs).
    """

    manifest_path = MODELS_DIR / "models-manifest.json"
    if manifest_path.exists():
        manifest = json.loads(manifest_path.read_text())
    else:
        manifest = {"version": "v2", "models": []}
    if manifest.get("version") != "v2":
        raise SystemExit(
            f"manifest at {manifest_path} is not v2 — refuse to clobber"
        )
    models = manifest.setdefault("models", [])

    entry = {
        "id": model_id,
        "task": "detect_open_vocab_text",
        "_comment": (
            "YOLOE (small) text-mode export. Prompts are baked into the "
            "graph at export time; per-camera config picks a subset at "
            "runtime. Regenerate via tools/models/gen_yoloe.py whenever "
            "the prompt vocabulary changes — the manifest sha256 below "
            "will refresh and the engine's loader will catch the diff."
        ),
        "input": {
            "width": input_w,
            "height": input_h,
            "channels": 3,
            "format": "RGB",
        },
        "default_thresholds": {
            # YOLOE logits run in the same range as YOLO-World — keep the
            # same conservative confidence floor; operators tune per-camera.
            "confidence": 0.10,
            "nms": 0.50,
        },
        "artifacts": [
            {
                "backend": "onnx",
                "path": artifact_path,
                "sha256": sha,
            }
        ],
        "presets": [
            {
                "name": str(input_w),
                "inputWidth": input_w,
                "inputHeight": input_h,
            }
        ],
        "default_preset": str(input_w),
        "prompts": prompts,
    }

    for i, m in enumerate(models):
        if m.get("id") == model_id:
            models[i] = entry
            break
    else:
        models.append(entry)

    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    print(
        f"[gen_yoloe] manifest upserted: {model_id} "
        f"({len(prompts)} prompts, sha {sha[:12]}…)"
    )


def export_yoloe(
    base_model: str,
    prompts: List[str],
    imgsz: int,
    opset: int,
    output: Path,
) -> None:
    """Run the ultralytics export with the prompt vocab baked in."""

    try:
        from ultralytics import YOLOE  # type: ignore
    except ImportError as ex:
        raise SystemExit(
            "ultralytics.YOLOE is unavailable in this venv. Upgrade with:\n"
            "    pip install -U 'git+https://github.com/ultralytics/ultralytics@main'\n"
            f"underlying error: {ex}"
        )

    print(f"[gen_yoloe] loading base model: {base_model}")
    model = YOLOE(base_model)
    print(f"[gen_yoloe] setting {len(prompts)} prompt classes")
    # Mirrors gen_yolo_world.py: `set_classes` bakes the open-vocab
    # vocabulary into the checkpoint as fixed text embeddings before
    # export. Post-call the model behaves like a closed-vocab YOLOv8
    # detector with C = len(prompts).
    model.set_classes(prompts)

    print(f"[gen_yoloe] exporting ONNX (imgsz={imgsz}, opset={opset})")
    model.export(
        format="onnx",
        dynamic=False,  # Static for predictability — open-vocab head
        # always runs at the configured input size.
        opset=opset,
        imgsz=imgsz,
        simplify=True,
        nms=False,  # keep raw YOLOv8 head — Rust postprocess does NMS.
    )

    base_stem = Path(base_model).stem
    base_dir = Path(base_model).resolve().parent
    candidates = [
        base_dir / f"{base_stem}.onnx",
        Path.cwd() / f"{base_stem}.onnx",
        REPO_ROOT / f"{base_stem}.onnx",
        MODELS_DIR / f"{base_stem}.onnx",
    ]
    src = next((p for p in candidates if p.is_file()), None)
    if src is None:
        raise SystemExit(
            f"export succeeded but ONNX not found in any of: {candidates}"
        )
    output.parent.mkdir(parents=True, exist_ok=True)
    if src.resolve() != output.resolve():
        shutil.copy2(src, output)
        try:
            os.unlink(src)
        except OSError:
            pass
        print(f"[gen_yoloe] copied {src} → {output}")


def smoke_check(onnx_path: Path, input_w: int, input_h: int) -> None:
    """Load the exported ONNX in onnxruntime and run a single zero tensor."""

    import numpy as np  # noqa: WPS433
    import onnxruntime as ort  # noqa: WPS433

    print(f"[gen_yoloe] smoke loading {onnx_path}")
    sess = ort.InferenceSession(
        str(onnx_path), providers=["CPUExecutionProvider"]
    )
    in_name = sess.get_inputs()[0].name
    in_shape = sess.get_inputs()[0].shape
    print(f"[gen_yoloe] input '{in_name}' shape={in_shape}")
    dummy = np.zeros((1, 3, input_h, input_w), dtype=np.float32)
    outputs = sess.run(None, {in_name: dummy})
    print(
        f"[gen_yoloe] smoke ok — got {len(outputs)} output(s); "
        f"first shape={outputs[0].shape}"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description="Export YOLOE text-mode ONNX")
    parser.add_argument(
        "--base-model",
        type=str,
        default=DEFAULT_BASE_MODEL,
        help=(
            "Ultralytics YOLOE checkpoint to start from. "
            "Defaults to yoloe-s.pt (small model — ~25 MB ONNX). "
            "Use yoloe-m.pt or larger for higher accuracy at the cost of fps."
        ),
    )
    parser.add_argument(
        "--prompts",
        type=Path,
        default=DEFAULT_PROMPTS,
        help=f"Prompt file (default: {DEFAULT_PROMPTS.relative_to(REPO_ROOT)})",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=DEFAULT_OUTPUT,
        help=f"Output ONNX path (default: {DEFAULT_OUTPUT.relative_to(REPO_ROOT)})",
    )
    parser.add_argument("--imgsz", type=int, default=640)
    parser.add_argument(
        "--opset",
        type=int,
        default=17,
        help=(
            "ONNX opset. 17 = YOLOE requires the gather + matmul ops the "
            "text-embedding fusion uses; 12 (the closed-vocab default) "
            "won't export."
        ),
    )
    parser.add_argument(
        "--manifest-id",
        type=str,
        default="yoloe26_s",
        help="ID written into models-manifest.json",
    )
    parser.add_argument(
        "--skip-smoke",
        action="store_true",
        help="Skip the onnxruntime smoke load (use only when debugging exports).",
    )
    args = parser.parse_args()

    try:
        prompts = read_prompts(args.prompts)
    except (FileNotFoundError, ValueError) as ex:
        print(f"[gen_yoloe] ERROR reading prompts: {ex}")
        return 1

    print(f"[gen_yoloe] prompts: {prompts}")
    try:
        export_yoloe(
            base_model=args.base_model,
            prompts=prompts,
            imgsz=args.imgsz,
            opset=args.opset,
            output=args.output,
        )
    except Exception as ex:  # noqa: BLE001
        print(f"[gen_yoloe] ERROR exporting: {ex}")
        return 1

    if not args.skip_smoke:
        try:
            smoke_check(args.output, args.imgsz, args.imgsz)
        except Exception as ex:  # noqa: BLE001
            print(f"[gen_yoloe] ERROR smoke-loading: {ex}")
            return 1

    sha = sha256_file(args.output)
    size_mb = args.output.stat().st_size / (1024 * 1024)
    print(f"[gen_yoloe] sha256 {sha}")
    print(f"[gen_yoloe] size   {size_mb:.2f} MB")
    upsert_manifest_entry(
        model_id=args.manifest_id,
        artifact_path=args.output.name,
        sha=sha,
        input_w=args.imgsz,
        input_h=args.imgsz,
        prompts=prompts,
    )
    print(f"[gen_yoloe] success: {args.output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
