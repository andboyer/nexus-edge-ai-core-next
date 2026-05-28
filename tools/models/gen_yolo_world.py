#!/usr/bin/env python3
"""
Generate the open-vocab YOLO-World ONNX used by M3's `YoloWorldDetector`.

This is the M3 counterpart of `gen_yolo26n.py`. The big difference is
**how prompts are wired**: YOLO-World takes a list of class prompts and
bakes them into the graph at *export* time as fixed text embeddings,
producing a YOLOv8-style detector with one class per prompt. Per-camera
config picks a *subset* of those prompts at runtime; the Rust detector
filters detections to that subset before emitting them.

Run from the workspace root with the model-gen venv active:

    source .venv-modelgen/bin/activate
    python tools/models/gen_yolo_world.py \\
        --prompts tools/models/yolo_world_default_prompts.txt

Output:
    models/yolo_world_v2_s.onnx   (~50–80 MB)

The prompt file lives under `tools/models/` (tracked) so the prompt
vocabulary is reproducible; the ONNX itself stays under `models/`
(gitignored) per the same policy as `yolo26n_{640,960,1280}.onnx`.
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
DEFAULT_OUTPUT = MODELS_DIR / "yolo_world_v2_s.onnx"
DEFAULT_PROMPTS = Path(__file__).resolve().parent / "yolo_world_default_prompts.txt"
DEFAULT_BASE_MODEL = "yolov8s-worldv2.pt"


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
    """Insert/update a YOLO-World entry in `models/models-manifest.json`.

    The manifest format is the v2 schema documented under
    nexus-edge-ai-core/models/MODEL_PACK_V2.md. We piggy-back on the
    same shape so a future Rust `ModelRegistry` port can parse one file
    for every model the engine knows about. Prompts ride in a custom
    `prompts` block (forward-compatible v2 extension; the v1 loader
    silently ignores unknown fields, which lets the same manifest serve
    both repos during the migration).
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
        "task": "detect_open_vocab",
        "_comment": (
            "YOLO-World v2 (small) export. Prompts are baked into the graph at "
            "export time; per-camera config picks a subset at runtime. "
            "Regenerate via tools/models/gen_yolo_world.py whenever the "
            "prompt vocabulary changes — the manifest sha256 below will "
            "refresh and the engine's loader will catch the diff."
        ),
        "input": {
            "width": input_w,
            "height": input_h,
            "channels": 3,
            "format": "RGB",
        },
        "default_thresholds": {
            "confidence": 0.10,  # YOLO-World logits run lower than v8/yolo26.
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
        # Forward-compatible: this block is YOLO-World specific. The v1 v2
        # loader ignores unknown top-level fields; the new Rust loader will
        # parse this block to seed `OpenVocabConfig.prompts`.
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
        f"[gen_yolo_world] manifest upserted: {model_id} "
        f"({len(prompts)} prompts, sha {sha[:12]}…)"
    )


def export_yolo_world(
    base_model: str,
    prompts: List[str],
    imgsz: int,
    opset: int,
    output: Path,
) -> None:
    """Run the ultralytics export with the prompt vocab baked in."""

    from ultralytics import YOLO  # type: ignore

    print(f"[gen_yolo_world] loading base model: {base_model}")
    model = YOLO(base_model)
    print(f"[gen_yolo_world] setting {len(prompts)} prompt classes")
    # `set_classes` is the ultralytics-supported way to bake an open-vocab
    # vocabulary into a YOLO-World checkpoint before export. After this
    # call the model behaves like a closed-vocab YOLO-v8 with C = len(prompts).
    model.set_classes(prompts)

    print(f"[gen_yolo_world] exporting ONNX (imgsz={imgsz}, opset={opset})")
    model.export(
        format="onnx",
        dynamic=False,  # Static for predictability; per-camera always
        # uses the same input dims for the open-vocab head.
        opset=opset,
        imgsz=imgsz,
        simplify=True,
        nms=False,  # keep raw YOLOv8 head — Rust postprocess does NMS.
    )

    # ultralytics writes `<base_stem>.onnx` next to the input checkpoint
    # (yes — *next to the .pt*, not in cwd). Sweep both spots.
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
        print(f"[gen_yolo_world] copied {src} → {output}")


def smoke_check(onnx_path: Path, input_w: int, input_h: int) -> None:
    """Load the exported ONNX in onnxruntime and run a single zero tensor.

    Catches export-time bugs (missing op, shape mismatch) inside the
    generator so a bad artifact never lands on disk silently.
    """

    import numpy as np  # noqa: WPS433
    import onnxruntime as ort  # noqa: WPS433

    print(f"[gen_yolo_world] smoke loading {onnx_path}")
    sess = ort.InferenceSession(
        str(onnx_path), providers=["CPUExecutionProvider"]
    )
    in_name = sess.get_inputs()[0].name
    in_shape = sess.get_inputs()[0].shape
    print(f"[gen_yolo_world] input '{in_name}' shape={in_shape}")
    dummy = np.zeros((1, 3, input_h, input_w), dtype=np.float32)
    outputs = sess.run(None, {in_name: dummy})
    print(
        f"[gen_yolo_world] smoke ok — got {len(outputs)} output(s); "
        f"first shape={outputs[0].shape}"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description="Export YOLO-World ONNX")
    parser.add_argument(
        "--base-model",
        type=str,
        default=DEFAULT_BASE_MODEL,
        help=(
            "Ultralytics YOLO-World checkpoint to start from. "
            "Defaults to yolov8s-worldv2.pt (small model — ~50 MB ONNX). "
            "Use yolov8m-worldv2.pt or larger for higher accuracy."
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
            "ONNX opset. 17 = YOLO-World requires the gather + matmul "
            "ops the text-embedding fusion uses; 12 (the closed-vocab "
            "default) won't export."
        ),
    )
    parser.add_argument(
        "--manifest-id",
        type=str,
        default="yolo_world_v2_s",
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
        print(f"[gen_yolo_world] ERROR reading prompts: {ex}")
        return 1

    print(f"[gen_yolo_world] prompts: {prompts}")
    try:
        export_yolo_world(
            base_model=args.base_model,
            prompts=prompts,
            imgsz=args.imgsz,
            opset=args.opset,
            output=args.output,
        )
    except Exception as ex:  # noqa: BLE001
        print(f"[gen_yolo_world] ERROR exporting: {ex}")
        return 1

    if not args.skip_smoke:
        try:
            smoke_check(args.output, args.imgsz, args.imgsz)
        except Exception as ex:  # noqa: BLE001
            print(f"[gen_yolo_world] ERROR smoke-loading: {ex}")
            return 1

    sha = sha256_file(args.output)
    size_mb = args.output.stat().st_size / (1024 * 1024)
    print(f"[gen_yolo_world] sha256 {sha}")
    print(f"[gen_yolo_world] size   {size_mb:.2f} MB")
    upsert_manifest_entry(
        model_id=args.manifest_id,
        artifact_path=args.output.name,
        sha=sha,
        input_w=args.imgsz,
        input_h=args.imgsz,
        prompts=prompts,
    )
    print(f"[gen_yolo_world] success: {args.output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
