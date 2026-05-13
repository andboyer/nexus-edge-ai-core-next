#!/usr/bin/env python3
"""
Generate `models/yolo26n_dynamic.onnx` — the closed-vocab YOLOv26-nano
detector that powers M1's `YoloOrtDetector`. Mirror of the v1 script
`models/generate_yolov26n_dynamic_onnx.py` from `nexus-edge-ai-core`,
just with the next-repo's path conventions and manifest writer.

Run from the workspace root with the model-gen venv active:

    source .venv-modelgen/bin/activate
    python tools/models/gen_yolo26n.py

Output:
    models/yolo26n_dynamic.onnx

The export uses dynamic input axes so a single ONNX serves the
320 / 640 / 1280 presets defined in `models/models-manifest.json`.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
MODELS_DIR = REPO_ROOT / "models"
DEFAULT_OUTPUT = MODELS_DIR / "yolo26n_dynamic.onnx"


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def update_manifest_sha(model_id: str, artifact_path: str, new_sha: str) -> None:
    """Patch the on-disk sha256 for one artifact in `models-manifest.json`.

    Idempotent — leaves the file untouched (and the file's mtime intact)
    if the sha already matches what we just computed.
    """

    manifest_path = MODELS_DIR / "models-manifest.json"
    if not manifest_path.exists():
        print(f"[gen_yolo26n] no manifest at {manifest_path}, skipping sha update")
        return
    manifest = json.loads(manifest_path.read_text())
    for model in manifest.get("models", []):
        if model.get("id") != model_id:
            continue
        for art in model.get("artifacts", []):
            if art.get("path") != artifact_path:
                continue
            if art.get("sha256") == new_sha:
                print(f"[gen_yolo26n] manifest sha already current ({new_sha[:12]}…)")
                return
            art["sha256"] = new_sha
            manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
            print(f"[gen_yolo26n] manifest sha updated → {new_sha[:12]}…")
            return
    print(f"[gen_yolo26n] no manifest entry matched (id={model_id} path={artifact_path})")


def main() -> int:
    parser = argparse.ArgumentParser(description="Export yolo26n_dynamic.onnx")
    parser.add_argument(
        "--output",
        type=Path,
        default=DEFAULT_OUTPUT,
        help=f"Output ONNX path (default: {DEFAULT_OUTPUT})",
    )
    parser.add_argument(
        "--imgsz",
        type=int,
        default=640,
        help="Anchor input size for the dynamic export (default 640)",
    )
    parser.add_argument(
        "--opset",
        type=int,
        default=12,
        help="ONNX opset version. 12 matches v1's pin for ORT 1.18 compat.",
    )
    args = parser.parse_args()

    try:
        from ultralytics import YOLO  # type: ignore
    except ImportError:
        print("[gen_yolo26n] ultralytics not installed.")
        print("[gen_yolo26n]   pip install -r tools/models/requirements.txt")
        return 1

    output: Path = args.output
    output.parent.mkdir(parents=True, exist_ok=True)
    print(f"[gen_yolo26n] downloading + exporting YOLOv26N → {output}")

    try:
        model = YOLO("yolov26n.pt")
        model.export(
            format="onnx",
            dynamic=True,
            opset=args.opset,
            imgsz=args.imgsz,
        )
    except Exception as ex:  # noqa: BLE001
        print(f"[gen_yolo26n] ERROR: export failed: {ex}")
        return 1

    # ultralytics writes `yolov26n.onnx` to the current working directory.
    candidates = [
        Path.cwd() / "yolov26n.onnx",
        REPO_ROOT / "yolov26n.onnx",
        MODELS_DIR / "yolov26n.onnx",
    ]
    src = next((p for p in candidates if p.is_file()), None)
    if src is None:
        print(f"[gen_yolo26n] ERROR: exported file not found in {candidates}")
        return 1
    if src.resolve() != output.resolve():
        shutil.copy2(src, output)
        try:
            os.unlink(src)
        except OSError:
            pass
        print(f"[gen_yolo26n] copied {src} → {output}")

    sha = sha256_file(output)
    print(f"[gen_yolo26n] sha256 {sha}")
    print(f"[gen_yolo26n] size   {output.stat().st_size / (1024 * 1024):.2f} MB")
    update_manifest_sha("yolo26n", "yolo26n_dynamic.onnx", sha)
    print(f"[gen_yolo26n] success: {output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
