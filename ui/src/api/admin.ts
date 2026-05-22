// Admin API wrappers (Phase 6 + M-Admin Phase 0): users, audit, runtime
// (bind / auth / OIDC discovery / diagnostics tarball).

import { api, getAccessToken } from "@/api/client";
import type {
  CreateUserRequest,
  CreateUserResponse,
  ListAuditQuery,
  ListAuditResponse,
  ListUsersResponse,
  ResetPasswordResponse,
  UpdateUserRequest,
  UserView,
} from "@/api/types";

// --- Users ----------------------------------------------------------------

export function listUsers(includeDeleted = false) {
  return api.get<ListUsersResponse>("/v1/admin/users", {
    query: includeDeleted ? { include_deleted: true } : undefined,
  });
}

export function createUser(req: CreateUserRequest) {
  return api.post<CreateUserResponse>("/v1/admin/users", req);
}

export function updateUser(id: string, req: UpdateUserRequest) {
  return api.put<UserView>(`/v1/admin/users/${encodeURIComponent(id)}`, req);
}

export function resetUserPassword(id: string) {
  return api.post<ResetPasswordResponse>(
    `/v1/admin/users/${encodeURIComponent(id)}/reset-password`,
    {},
  );
}

export function unlockUser(id: string) {
  return api.post<void>(
    `/v1/admin/users/${encodeURIComponent(id)}/unlock`,
    {},
  );
}

export function deleteUser(id: string) {
  return api.delete<void>(`/v1/admin/users/${encodeURIComponent(id)}`);
}

// --- Audit ----------------------------------------------------------------

export function listAudit(q: ListAuditQuery = {}) {
  return api.get<ListAuditResponse>("/v1/admin/audit", {
    query: q as Record<string, string | number | boolean | undefined>,
  });
}

// --- Server bind (M-Admin Phase 0, restart-based) -------------------------

export interface ServerBindOut {
  current: string;
  pending: string | null;
}

export interface PutServerBindOut {
  current: string;
  pending: string;
  restart_required: boolean;
}

export function getServerBind() {
  return api.get<ServerBindOut>("/v1/admin/server/bind");
}

export function putServerBind(addr: string) {
  return api.put<PutServerBindOut>("/v1/admin/server/bind", { addr });
}

// --- Auth config (M-Admin Phase 0, restart-based) -------------------------
//
// We use `unknown` for the wire AuthConfig shape on PUT because the engine
// is the source of truth for the schema — the UI builds it from the
// individual form fields and we don't want a long-lived parallel TS type.

export type AuthMode = "local" | "oidc" | "hybrid";

export interface SafeOidcConfig {
  issuer: string;
  audience: string;
  jwks_uri?: string;
  client_id?: string;
  display_name?: string;
  scopes: string[];
  role_claims: string[];
  deny_unmapped: boolean;
  redirect_uri?: string;
}

export interface SafeAuthConfig {
  mode: AuthMode;
  oidc?: SafeOidcConfig;
  admin_secret_path?: string;
}

export interface AuthConfigOut {
  current: SafeAuthConfig;
  pending: SafeAuthConfig | null;
}

export interface PutAuthConfigOut {
  restart_required: boolean;
  mode: AuthMode;
  oidc_issuer?: string;
}

export function getAuthConfig() {
  return api.get<AuthConfigOut>("/v1/admin/auth/config");
}

export function putAuthConfig(body: unknown) {
  return api.put<PutAuthConfigOut>("/v1/admin/auth/config", body);
}

// --- OIDC discovery dry-run -----------------------------------------------

export interface TestDiscoveryOut {
  ok: boolean;
  issuer: string;
  authorization_endpoint: string;
  token_endpoint: string;
  jwks_uri: string;
  userinfo_endpoint?: string;
  supports_pkce_s256: boolean;
}

export function testOidcDiscovery(req: {
  issuer: string;
  audience?: string;
  jwks_uri?: string;
}) {
  return api.post<TestDiscoveryOut>(
    "/v1/admin/auth/oidc/test-discovery",
    req,
  );
}

// --- Diagnostics tarball (streaming) --------------------------------------
//
// The endpoint streams `Content-Type: application/gzip` with a
// `Content-Disposition: attachment; filename="..."` header. We use the
// `asBlob` codepath so the entire response body buffers as a Blob ready
// to hand to URL.createObjectURL, then derive the filename from the
// header so the saved file matches what the engine generated.

export interface DiagnosticsBundle {
  blob: Blob;
  filename: string;
}

export async function downloadDiagnosticsBundle(
  query?: { audit_limit?: number; motion_limit?: number },
): Promise<DiagnosticsBundle> {
  // We can't use `api.get<Blob>(..., { asBlob: true })` here because we
  // also need the Content-Disposition header to recover the engine's
  // intended filename. Drop down to fetch + manually attach the bearer
  // token via the shared accessor in the client module.
  const params = new URLSearchParams();
  if (query?.audit_limit !== undefined) {
    params.set("audit_limit", String(query.audit_limit));
  }
  if (query?.motion_limit !== undefined) {
    params.set("motion_limit", String(query.motion_limit));
  }
  const qs = params.toString();
  const url = `/api/v1/admin/diagnostics/export${qs ? `?${qs}` : ""}`;

  const token = getAccessToken();
  const res = await fetch(url, {
    headers: token ? { Authorization: `Bearer ${token}` } : {},
  });
  if (!res.ok) {
    throw new Error(
      `diagnostics export failed: HTTP ${res.status} ${res.statusText}`,
    );
  }
  const blob = await res.blob();
  const filename = parseContentDispositionFilename(
    res.headers.get("content-disposition"),
  ) ?? `nexus-diagnostics-${nowStamp()}.tar.gz`;
  return { blob, filename };
}

function parseContentDispositionFilename(header: string | null): string | null {
  if (!header) return null;
  // Match the simple `filename="foo.tar.gz"` form the engine emits.
  const m = /filename\s*=\s*"?([^";]+)"?/i.exec(header);
  if (!m || m[1] === undefined) return null;
  return m[1].trim();
}

function nowStamp(): string {
  const d = new Date();
  const pad = (n: number) => String(n).padStart(2, "0");
  return (
    `${d.getUTCFullYear()}${pad(d.getUTCMonth() + 1)}${pad(d.getUTCDate())}-`
    + `${pad(d.getUTCHours())}${pad(d.getUTCMinutes())}${pad(d.getUTCSeconds())}`
  );
}


// --- Storage watermarks (M-Admin Phase 0, restart-based) ------------------

export interface WatermarkOut {
  low_pct: number;
  panic_pct: number;
  pending_low_pct: number | null;
  pending_panic_pct: number | null;
}

export interface PutWatermarkOut {
  current_low_pct: number;
  current_panic_pct: number;
  pending_low_pct: number;
  pending_panic_pct: number;
  restart_required: boolean;
}

export function getWatermarks() {
  return api.get<WatermarkOut>("/v1/admin/server/watermarks");
}

export function putWatermarks(low_pct: number, panic_pct: number) {
  return api.put<PutWatermarkOut>("/v1/admin/server/watermarks", {
    low_pct,
    panic_pct,
  });
}


// --- Default inference model (M-Admin Phase 0 follow-up, restart-based) ---

export interface InferenceModelView {
  kind: string;
  preset: string;
  input_width: number;
  input_height: number;
  score_threshold: number;
  pack_path?: string;
}

export interface InferenceModelOut {
  current: InferenceModelView;
  pending: InferenceModelView | null;
  available_kinds: string[];
}

export interface PutInferenceModelOut {
  current: InferenceModelView;
  pending: InferenceModelView;
  restart_required: boolean;
}

export interface InferenceModelPatch {
  kind?: string;
  preset?: string;
  input_width?: number;
  input_height?: number;
  score_threshold?: number;
  /** Empty string clears any persisted pack_path. */
  pack_path?: string;
}

export function getInferenceModel() {
  return api.get<InferenceModelOut>("/v1/admin/server/inference");
}

export function putInferenceModel(patch: InferenceModelPatch) {
  return api.put<PutInferenceModelOut>("/v1/admin/server/inference", patch);
}


// --- Engine self-restart (M-Admin Phase 0 follow-up) ----------------------

export interface RestartOut {
  restart_scheduled: boolean;
  delay_ms: number;
  current_bind: string;
}

export function restartEngine(delay_ms?: number) {
  return api.post<RestartOut>(
    "/v1/admin/server/restart",
    delay_ms === undefined ? {} : { delay_ms },
  );
}
