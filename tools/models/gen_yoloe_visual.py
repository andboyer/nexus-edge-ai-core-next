#!/usr/bin/env python3
"""
Generate the YOLOE **visual-prompt** image encoder ONNX used by M3.1's
`YoloeVisualDetector` and the engine-side `ImageEncoder`.

Where `gen_yoloe.py` bakes text prompts into the detector graph,
visual-mode YOLOE ships TWO ONNX files:

    yoloe26_s.onnx                  # detector (visual-mode head)
    yoloe26_s_image_encoder.onnx    # standalone image encoder

The detector takes (image, visual_prompt_embeddings[N,D]) and the
encoder produces the [D]-dimensional embedding for one reference crop.
The encoder runs offline in the admin upload path (per visual prompt,
ONCE); the detector runs hot per-frame with the current camera's
attached embeddings stacked into the VPE tensor.

This script exports the encoder half. Run AFTER `gen_yoloe.py` so the
detector pack is on disk first — the encoder must be packed next to it
under `models/<pack>/yoloe26_s_image_encoder.onnx` for
`VisualPromptsAdminState::from_config` to find it.

Run from the workspace root with the model-gen venv active:

    source .venv-modelgen/bin/activate
    python tools/models/gen_yoloe_visual.py

Output:
    models/yoloe26_s_image_encoder.onnx   (~15–20 MB)

╭─ STATUS (verified May 2026 against ultralytics 8.4.50) ──────────╮
│                                                                  │
│ This export does NOT work against the public ultralytics release │
│ currently pinned in `.venv-modelgen/requirements.txt`. Two       │
│ blockers, both upstream:                                         │
│                                                                  │
│   1. `YOLOE.export(visual_prompt_encoder=True)` is unknown — the │
│      kwarg is rejected by `cfg/__init__.py`'s arg validator with │
│      a ValueError. The preferred-path call below fails.          │
│                                                                  │
│   2. The fallback hand-export of `model.model.image_encoder` /   │
│      `.visual_prompt_encoder` also fails — neither submodule     │
│      exists on `YOLOESegModel`. Visual-prompt embeddings are     │
│      produced by `model.model.get_visual_pe(img, visual)`, which │
│      runs the FULL backbone with a region-selector tensor and is │
│      not a separable encoder.                                    │
│                                                                  │
│ Until ultralytics ships a separable image-encoder export (PR     │
│ pending in https://github.com/ultralytics/ultralytics), the      │
│ engine's `kind = "yoloe_visual"` runtime is dormant — no camera  │
│ in the dev config uses it. `kind = "yoloe"` (text-prompt mode)   │
│ is fully operable and is the recommended open-vocab kind today.  │
│                                                                  │
│ When upstream support lands, re-run this script as documented;   │
│ the model store + manifest wiring in this file is correct, only  │
│ the underlying export call needs upstream backing.               │
│                                                                  │
╰──────────────────────────────────────────────────────────────────╯

The Rust side only cares about the on-disk ONNX shape (input:
`[1,3,H,W]` float32 RGB, normalized 0..1; output: `[1,D]` float32) —
any export that produces that shape works.
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
DEFAULT_OUTPUT = MODELS_DIR / "yoloe26_s_image_encoder.onnx"
# Ultralytics 8.4.x ships YOLOE only as `-seg` checkpoints (see gen_yoloe.py
# for the same workaround on the detector side). Pick the 26-arch small
# variant — must match the detector checkpoint used by gen_yoloe.py so the
# embedding dim baked into the detector head agrees with what this encoder
# produces.
DEFAULT_BASE_MODEL = "yoloe-26s-seg.pt"
# YOLOE small head emits a 512-dim image embedding; medium = 768, etc.
# The dim must match what `gen_yoloe.py` baked into the detector head,
# and must match `NEXUS_WORKER_EMBEDDING_DIM` at worker startup.
DEFAULT_EMBEDDING_DIM = 512


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def upsert_manifest_entry(
    *,
    model_id: str,
    artifact_path: str,
    sha: str,
    input_w: int,
    input_h: int,
    embedding_dim: int,
) -> None:
    """Insert/update the encoder entry in `models/models-manifest.json`."""

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
        "task": "embed_image",
        "_comment": (
            "YOLOE visual-prompt image encoder. Produces a fixed-dim "
            "embedding from a reference crop. Used offline by the engine "
            "admin upload path; per-camera detection runs the detector "
            "ONNX with the stacked embeddings of the attached prompts."
        ),
        "input": {
            "width": input_w,
            "height": input_h,
            "channels": 3,
            "format": "RGB",
        },
        "artifacts": [
            {
                "backend": "onnx",
                "path": artifact_path,
                "sha256": sha,
            }
        ],
        "embedding_dim": embedding_dim,
    }

    for i, m in enumerate(models):
        if m.get("id") == model_id:
            models[i] = entry
            break
    else:
        models.append(entry)

    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    print(
        f"[gen_yoloe_visual] manifest upserted: {model_id} "
        f"(dim={embedding_dim}, sha {sha[:12]}…)"
    )


def export_encoder(
    base_model: str,
    imgsz: int,
    opset: int,
    output: Path,
) -> None:
    """Export the YOLOE image-encoder branch as a standalone ONNX."""

    try:
        from ultralytics import YOLOE  # type: ignore
    except ImportError as ex:
        raise SystemExit(
            "ultralytics.YOLOE is unavailable in this venv. Upgrade with:\n"
            "    pip install -U 'git+https://github.com/ultralytics/ultralytics@main'\n"
            f"underlying error: {ex}"
        )

    print(f"[gen_yoloe_visual] loading base model: {base_model}")
    model = YOLOE(base_model)

    # The upstream public surface for exporting JUST the visual-prompt
    # image-encoder head is still settling. We try the documented path
    # first; if that's not present, fall back to manual torch export of
    # the encoder submodule.
    print(f"[gen_yoloe_visual] exporting image encoder (imgsz={imgsz}, opset={opset})")
    try:
        # Preferred path: ultralytics ≥ the YOLOE visual PR.
        model.export(
            format="onnx",
            dynamic=False,
            opset=opset,
            imgsz=imgsz,
            simplify=True,
            visual_prompt_encoder=True,  # type: ignore[arg-type]
        )
        candidate_stems = [
            f"{Path(base_model).stem}_image_encoder",
            f"{Path(base_model).stem}-visual-encoder",
            Path(base_model).stem,
        ]
    except Exception as ex:  # noqa: BLE001
        # Ultralytics 8.4.x raises a custom ValueError ("'visual_prompt_encoder'
        # is not a valid YOLO argument") rather than TypeError, so we widen the
        # catch and fall through to a manual torch export of the encoder
        # submodule. SystemExit raised inside the fallback (encoder-missing)
        # still propagates because it derives from BaseException, not Exception.
        print(
            f"[gen_yoloe_visual] preferred export path unavailable ({ex}); "
            "falling back to torch.onnx.export on encoder submodule"
        )
        import torch  # noqa: WPS433

        encoder = getattr(model.model, "image_encoder", None) or getattr(
            model.model, "visual_prompt_encoder", None
        )
        if encoder is None:
            raise SystemExit(
                "YOLOE visual encoder cannot be exported against the installed "
                "ultralytics version. Neither `model.export(visual_prompt_encoder=True)` "
                "nor a separable `model.model.image_encoder` / `.visual_prompt_encoder` "
                "submodule is available. Visual prompts are produced by "
                "`model.model.get_visual_pe(img, visual)` which runs the full backbone "
                "with a region-selector tensor and cannot be cleanly exported as a "
                "standalone image encoder.\n"
                "\n"
                "→ Track upstream ultralytics for a `visual_prompt_encoder` export flag.\n"
                "→ Until then, the engine's `kind = \"yoloe_visual\"` runtime is dormant. "
                "Use `kind = \"yoloe\"` (text-prompt mode) for open-vocab detection."
            )
        dummy = torch.zeros((1, 3, imgsz, imgsz))
        encoder.eval()
        torch.onnx.export(
            encoder,
            dummy,
            str(output),
            input_names=["image"],
            output_names=["embedding"],
            opset_version=opset,
            do_constant_folding=True,
        )
        candidate_stems = [output.stem]

    base_dir = Path(base_model).resolve().parent
    candidates = [
        base_dir / f"{stem}.onnx"
        for stem in candidate_stems
    ] + [
        Path.cwd() / f"{stem}.onnx" for stem in candidate_stems
    ] + [
        REPO_ROOT / f"{stem}.onnx" for stem in candidate_stems
    ] + [
        MODELS_DIR / f"{stem}.onnx" for stem in candidate_stems
    ]
    if output.is_file() and output.stat().st_size > 0:
        # Manual fallback path already wrote `output` directly.
        return
    src = next((p for p in candidates if p.is_file()), None)
    if src is None:
        raise SystemExit(
            f"export succeeded but encoder ONNX not found in any of: {candidates}"
        )
    output.parent.mkdir(parents=True, exist_ok=True)
    if src.resolve() != output.resolve():
        shutil.copy2(src, output)
        try:
            os.unlink(src)
        except OSError:
            pass
        print(f"[gen_yoloe_visual] copied {src} → {output}")


def smoke_check(onnx_path: Path, input_w: int, input_h: int, expected_dim: int) -> None:
    """Load the encoder ONNX and verify the output is `[1, expected_dim]`."""

    import numpy as np  # noqa: WPS433
    import onnxruntime as ort  # noqa: WPS433

    print(f"[gen_yoloe_visual] smoke loading {onnx_path}")
    sess = ort.InferenceSession(
        str(onnx_path), providers=["CPUExecutionProvider"]
    )
    in_name = sess.get_inputs()[0].name
    in_shape = sess.get_inputs()[0].shape
    print(f"[gen_yoloe_visual] input '{in_name}' shape={in_shape}")
    dummy = np.zeros((1, 3, input_h, input_w), dtype=np.float32)
    outputs = sess.run(None, {in_name: dummy})
    out_shape = outputs[0].shape
    print(f"[gen_yoloe_visual] smoke ok — output shape={out_shape}")
    if len(out_shape) != 2 or out_shape[0] != 1 or out_shape[1] != expected_dim:
        raise SystemExit(
            f"encoder output shape {out_shape} does not match expected "
            f"[1, {expected_dim}] — update --embedding-dim or re-export"
        )


def main() -> int:
    parser = argparse.ArgumentParser(description="Export YOLOE image encoder ONNX")
    parser.add_argument(
        "--base-model",
        type=str,
        default=DEFAULT_BASE_MODEL,
        help=(
            "Ultralytics YOLOE checkpoint. MUST match the checkpoint used "
            "for the detector export so embedding dims line up."
        ),
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=DEFAULT_OUTPUT,
        help=f"Output ONNX path (default: {DEFAULT_OUTPUT.relative_to(REPO_ROOT)})",
    )
    parser.add_argument("--imgsz", type=int, default=640)
    parser.add_argument("--opset", type=int, default=17)
    parser.add_argument(
        "--embedding-dim",
        type=int,
        default=DEFAULT_EMBEDDING_DIM,
        help=(
            "Expected output embedding dim. Must match the detector export "
            "AND `NEXUS_WORKER_EMBEDDING_DIM` (or the inferred default 512)."
        ),
    )
    parser.add_argument(
        "--manifest-id",
        type=str,
        default="yoloe26_s_image_encoder",
        help="ID written into models-manifest.json",
    )
    parser.add_argument(
        "--skip-smoke",
        action="store_true",
        help="Skip the onnxruntime smoke load (use only when debugging exports).",
    )
    args = parser.parse_args()

    try:
        export_encoder(
            base_model=args.base_model,
            imgsz=args.imgsz,
            opset=args.opset,
            output=args.output,
        )
    except Exception as ex:  # noqa: BLE001
        print(f"[gen_yoloe_visual] ERROR exporting: {ex}")
        return 1

    if not args.skip_smoke:
        try:
            smoke_check(args.output, args.imgsz, args.imgsz, args.embedding_dim)
        except Exception as ex:  # noqa: BLE001
            print(f"[gen_yoloe_visual] ERROR smoke-loading: {ex}")
            return 1

    sha = sha256_file(args.output)
    size_mb = args.output.stat().st_size / (1024 * 1024)
    print(f"[gen_yoloe_visual] sha256 {sha}")
    print(f"[gen_yoloe_visual] size   {size_mb:.2f} MB")
    upsert_manifest_entry(
        model_id=args.manifest_id,
        artifact_path=args.output.name,
        sha=sha,
        input_w=args.imgsz,
        input_h=args.imgsz,
        embedding_dim=args.embedding_dim,
    )
    print(f"[gen_yoloe_visual] success: {args.output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
