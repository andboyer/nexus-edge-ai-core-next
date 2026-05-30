# xtask — repo maintenance tools

`cargo xtask <subcommand>` runs ad-hoc tools that don't belong in any
runtime crate. The `xtask = "run --package xtask --bin xtask --"`
alias in [`../.cargo/config.toml`](../.cargo/config.toml) keeps the
invocation short.

## `cargo xtask check-models`

Validates [`../models/models-manifest.json`](../models/models-manifest.json)
against the model-license + product-invariant rules recorded in
[`../AGENTS.md`](../AGENTS.md) rule 2:

| Rule | Behavior |
| --- | --- |
| Face-recognition denylist substrings in model `id` or artifact `path` (`AdaFace`, `ArcFace`, `InsightFace`, `Buffalo`, `FaceNet`, `SphereFace`, `CosFace`, `MagFace`) | **Hard error.** These never ship at the edge in v1. |
| Explicit license / dataset-license deny values (`non-commercial`, `nc-4.0`, `cc-by-nc`, `research`, `research-only`, `unknown`, `proprietary`) | **Hard error.** Case-insensitive substring match. |
| Missing `license` or `weights_dataset_license` on any entry | **Warning** by default; **error** under `--strict`. Lets the gate land before every legacy entry is back-filled. |

Exits non-zero on any error (or warning under `--strict`).

### CI integration

```yaml
- name: check models
  run: cargo xtask check-models
```

Flip to `--strict` once every model entry in
`models/models-manifest.json` declares both `license` and
`weights_dataset_license`.
