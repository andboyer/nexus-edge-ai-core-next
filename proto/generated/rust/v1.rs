// @generated — DO NOT EDIT BY HAND
// Regenerate with `cargo xtask gen-proto` from proto/v1.json.
//
// Source schema: Nexus edge↔cloud wire protocol
// Canonical schema for v1 of the wire envelope and the eight message kinds Phase 1 needs (heartbeat, heartbeat_ack, alert, alert_ack, entitlement_update, rpc_call, rpc_response, close_session). HUMAN-EDITED source of truth. Rust types live in proto/generated/rust/v1.rs; TypeScript zod schemas in proto/generated/ts/v1.ts. `cargo xtask gen-proto` regenerates both; CI fails if they're stale.

use serde::{Deserialize, Serialize};

/// Structural shape of the JWT body inside ActorTokenJwt. Not transmitted standalone — included in this schema so codegen produces a typed verifier struct on the edge side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActorTokenClaims {
    pub aud: String,
    pub core_id: Uuid,
    /// MUST be iat + 30 s.
    pub exp: UnixSeconds,
    pub http_method: String,
    pub iat: UnixSeconds,
    /// https://entitlement.nexus.example
    pub iss: String,
    pub jti: Uuid,
    pub org_id: Uuid,
    /// Exact match against rpc_call.payload.path.
    pub path: String,
    pub role: String,
    /// User UUID, or `system:<svc-name>`.
    pub sub: String,
}

/// Compact JWS (Ed25519). Header `{alg:"EdDSA", kid:<keyvault-key-id>}`. Claims per WIRE_PROTOCOL.md §11.2; engine verifies before applying any state-mutating rpc_call. See ActorTokenClaims for the structural shape of the inner JWT body.
pub type ActorTokenJwt = String;

/// Cloud → Edge. permanent_failure tells the edge outbox to mark the row suppressed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertAckPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub status: String,
}

/// Edge → Cloud. AlertEvent — shape mirrors nexus-types/src/lib.rs on the edge side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertPayload {
    /// Phase 21.2 — clip pre-attached on edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attached_history: Option<bool>,
    /// [x, y, w, h] normalised 0..1
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bbox: Option<Vec<f64>>,
    /// Per-core integer id (matches cameras.edge_camera_id).
    pub camera_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip_blob_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// Core-local id. Dedup key on cloud INSERT (cores.id × edge_event_id).
    pub edge_event_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_rule_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_label: Option<String>,
    pub severity: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_blob_url: Option<String>,
    pub ts: Timestamp,
}

/// Cloud → Edge. Server-initiated clean disconnect. Edge reconnects immediately.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloseSessionPayload {
    pub reason: String,
}

/// Cloud → Edge. Push triggered by Stripe webhook or initial enrollment. Edge persists + applies immediately.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntitlementUpdatePayload {
    /// Compact JWS. Verified with the entitlement public key bundled at enrollment.
    pub jwt: String,
}

/// Cloud → Edge. Reply to a heartbeat. May hint at cert rotation after day 75.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeartbeatAckPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert_rotate: Option<HeartbeatAckPayloadCertRotate>,
    pub server_ts: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeartbeatAckPayloadCertRotate {
    pub reason: String,
}

/// Edge → Cloud. Sent every 30 s. Minimum health snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeartbeatPayload {
    /// Phase 1.15: edge wall-clock for skew tracking (gateway writes EMA to cores.last_skew_ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_ts_unix_ms: Option<u64>,
    pub online_cameras: u64,
    pub queued_alerts: u64,
    /// Phase 7: OTA-update status block. Omitted by pre-Phase-7 edges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<ReleaseStatus>,
    /// Hardware tier from config/tiers/.
    pub tier: String,
    pub uptime_s: u64,
    /// Engine semver, e.g. "0.5.0".
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseStatus {
    pub channel: String,
    pub current_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_update_attempt_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_update_result: Option<String>,
    pub recording_active: bool,
}

/// Cloud → Edge. Proxies an HTTP call to the edge's loopback admin API. State-mutating methods MUST carry actor_token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcCallPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_token: Option<ActorTokenJwt>,
    /// Optional request body. Base64 of the raw bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<std::collections::BTreeMap<String, String>>,
    pub method: String,
    /// Absolute path on the edge loopback admin API.
    pub path: String,
    /// Phase 1.16: propagated from HTTP Idempotency-Key for end-to-end dedup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<Uuid>,
}

/// Shape of body on 4xx/5xx responses. See WIRE_PROTOCOL.md §4.3 for the full code catalogue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcErrorBody {
    pub error: String,
    pub message: String,
}

/// Edge → Cloud. Reply to an rpc_call. Uses envelope.in_reply_to to bind to the original.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcResponsePayload {
    pub body: serde_json::Value,
    pub status: u64,
}

pub type Timestamp = String;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceContext {
    /// 8-byte W3C span-id, hex-encoded.
    pub parent_span_id: String,
    /// 16-byte W3C trace-id, hex-encoded.
    pub trace_id: String,
}

pub type UnixSeconds = u64;

pub type Uuid = String;

/// Envelope metadata — every field of [`Envelope`] except the
/// `kind`/`payload` discriminator, which is encoded by [`EnvelopeBody`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvelopeMeta {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<TraceContext>,
    pub ts: Timestamp,
    pub v: i64,
}

/// Tagged-union body of every [`Envelope`]. Serde writes `kind` +
/// `payload` as siblings of the envelope-meta fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum EnvelopeBody {
    Heartbeat(HeartbeatPayload),
    HeartbeatAck(HeartbeatAckPayload),
    Alert(AlertPayload),
    AlertAck(AlertAckPayload),
    EntitlementUpdate(EntitlementUpdatePayload),
    RpcCall(RpcCallPayload),
    RpcResponse(RpcResponsePayload),
    CloseSession(CloseSessionPayload),
}

/// One WebSocket text frame on the wire. See the schema header for invariants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    #[serde(flatten)]
    pub meta: EnvelopeMeta,
    #[serde(flatten)]
    pub body: EnvelopeBody,
}
