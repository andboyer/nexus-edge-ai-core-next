# Wire protocol — `proto/`

> **The canonical schema is [`v1.json`](v1.json) (JSON Schema 2020-12).** It is the
> source of truth. Rust + TypeScript types in `generated/` are emitted from it by
> [`cargo xtask gen-proto`](../xtask/src/proto.rs); CI fails if they're stale.

## Files

| File | Purpose |
|---|---|
| `v1.json`                  | Hand-edited schema. The only file you change to evolve the protocol. |
| `generated/rust/v1.rs`     | Rust types + tagged-enum `Envelope`. `#[derive(Serialize, Deserialize)]`. |
| `generated/ts/v1.ts`       | TypeScript types + zod schemas. Consumed by `cloud-ui/` and `cms-ui/`. |
| `generated/CHECKSUM`       | SHA-256 of `v1.json` at the time of last generation. CI uses it to detect drift. |

## Workflow

```bash
# After editing v1.json:
cargo xtask gen-proto

# Verify generated/ matches v1.json (CI runs this on every PR):
cargo xtask gen-proto --check

# Copy the Rust output into the core repo's vendored crate:
cargo xtask sync-cloud-protocol --core ../nexus-edge-ai-core-next
```

## Versioning

- `v` in [`v1.json`](v1.json#L42) is the protocol version.
- Breaking changes bump `v` and need a new schema file `proto/v2.json` (not an edit of v1).
- `edge-gateway` supports `N` and `N-1` per [`docs/WIRE_PROTOCOL.md §3`](../docs/WIRE_PROTOCOL.md#3-versioning).

## Boundaries

This directory is the **only** sanctioned shared shape between the cloud-console repo
and [nexus-edge-ai-core-next](../../nexus-edge-ai-core-next). The core repo vendors
[`generated/rust/v1.rs`](generated/rust/v1.rs) as `crates/nexus-cloud-protocol/src/v1.rs`
and validates the checksum at build time. See
[`docs/REPO_BOUNDARY.md R3`](../docs/REPO_BOUNDARY.md#r3-the-wire-protocol-schema-lives-in-this-repo-only).
