// M6 Phase 2 Step 2.9 — session + legacy bearer state.
//
// Two coexisting auth shapes:
//
// 1. **Legacy dev token** (Phase 0): a single string in
//    `localStorage["nexus_admin_token"]`, set via the topbar
//    paste-field. Used under `auth.mode in {none, dev_token}`.
//
// 2. **Session** (this step): an `access_token + refresh_token +
//    user` triple in `localStorage["nexus_session"]`, set by
//    the login overlay. Used under `auth.mode in {local, oidc,
//    hybrid}`. The access token is short-lived (15 min by
//    default); on 401 we silently refresh once before retrying.
//
// `authHeader()` prefers the session over the legacy token —
// once a user has logged in, the topbar paste-field is hidden
// anyway, but the precedence makes the boot order irrelevant.
//
// The status pill next to the (mode-dependent) topbar control
// is driven by `reportRequestOutcome`: green on the first 2xx
// of a gated write when *some* credential is set, red on any
// 401/403, unknown otherwise.

import { auth as authApi } from "../api/auth.js";
import type {
  AuthInfoResponse,
  AuthMode,
  SessionUser,
  TokenResponse,
} from "../api/types.js";

const LEGACY_KEY = "nexus_admin_token";
const SESSION_KEY = "nexus_session";

export type AuthStatus = "unknown" | "ok" | "unauthorized";
type StatusListener = (s: AuthStatus) => void;
type SessionListener = (s: Session | null) => void;
type AuthInfoListener = (info: AuthInfoResponse | null) => void;

const statusListeners = new Set<StatusListener>();
const sessionListeners = new Set<SessionListener>();
const authInfoListeners = new Set<AuthInfoListener>();

let status: AuthStatus = "unknown";

/// Locally-cached projection of the engine's `/v1/auth/info`
/// probe. Populated once at boot by `loadAuthInfo()`. Mutating
/// `auth.mode` in `nexus.toml` requires an engine restart AND a
/// page reload to take effect on the SPA — same posture as
/// every other static config.
let cachedAuthInfo: AuthInfoResponse | null = null;

/// In-flight refresh promise dedupe — under burst load the SPA
/// can fire many parallel 401s; we only want one refresh round-
/// trip. Every subsequent caller awaits the same promise.
let inflightRefresh: Promise<Session | null> | null = null;

// ---------------------------------------------------------------------------
// Session shape (browser-side only — the wire types live in `api/types.ts`).
// ---------------------------------------------------------------------------

export interface Session {
  access_token: string;
  refresh_token: string;
  /// Epoch-millis after which the access token is expired. We
  /// store the absolute deadline (not `expires_in`) so clock
  /// drift after a tab sleep doesn't quietly keep using a dead
  /// token.
  access_expires_at: number;
  refresh_expires_at: number;
  user: SessionUser;
}

// ---------------------------------------------------------------------------
// Legacy bearer (Phase 0) — unchanged contract.
// ---------------------------------------------------------------------------

export function getToken(): string | null {
  try {
    const v = localStorage.getItem(LEGACY_KEY);
    return v && v.trim() !== "" ? v.trim() : null;
  } catch {
    return null;
  }
}

export function setToken(token: string | null): void {
  try {
    if (token == null || token.trim() === "") {
      localStorage.removeItem(LEGACY_KEY);
    } else {
      localStorage.setItem(LEGACY_KEY, token.trim());
    }
  } catch {
    // localStorage may be disabled (private mode) — silent
    // no-op so the rest of the UI keeps working with an
    // in-memory-only token.
  }
  publishStatus("unknown");
}

// ---------------------------------------------------------------------------
// Session (Step 2.9) — read/write/observe.
// ---------------------------------------------------------------------------

export function getSession(): Session | null {
  try {
    const raw = localStorage.getItem(SESSION_KEY);
    if (!raw) return null;
    const s = JSON.parse(raw) as Session;
    // Validate the shape just enough to fail closed on a hand-
    // mangled value (e.g. someone bumped the schema).
    if (
      typeof s !== "object" ||
      typeof s.access_token !== "string" ||
      typeof s.refresh_token !== "string" ||
      typeof s.access_expires_at !== "number" ||
      typeof s.refresh_expires_at !== "number" ||
      typeof s.user !== "object" ||
      s.user == null
    ) {
      return null;
    }
    return s;
  } catch {
    return null;
  }
}

export function setSession(s: Session | null): void {
  try {
    if (s == null) {
      localStorage.removeItem(SESSION_KEY);
    } else {
      localStorage.setItem(SESSION_KEY, JSON.stringify(s));
    }
  } catch {
    // Private mode — silent no-op.
  }
  publishSession(s);
  publishStatus(s ? "ok" : "unknown");
}

/// Build a Session from the engine's `TokenResponse`. Pure
/// helper — does NOT persist.
export function sessionFromTokenResponse(t: TokenResponse): Session {
  const now = Date.now();
  return {
    access_token: t.access_token,
    refresh_token: t.refresh_token,
    access_expires_at: now + t.expires_in * 1000,
    refresh_expires_at: now + t.refresh_expires_in * 1000,
    user: t.user,
  };
}

export function onSessionChange(fn: SessionListener): () => void {
  sessionListeners.add(fn);
  fn(getSession());
  return () => {
    sessionListeners.delete(fn);
  };
}

function publishSession(s: Session | null): void {
  for (const fn of sessionListeners) fn(s);
}

// ---------------------------------------------------------------------------
// Outgoing header — session wins over legacy bearer.
// ---------------------------------------------------------------------------

export function authHeader(): Record<string, string> {
  const s = getSession();
  if (s) {
    // Send the bearer even if access_expires_at is past — the
    // engine will return 401 and `client.ts::request` will
    // call `tryRefresh()` then retry. If we suppressed the
    // bearer here the audit log would carry "no bearer" for
    // every refresh-window request, which is misleading.
    return { Authorization: `Bearer ${s.access_token}` };
  }
  const t = getToken();
  return t ? { Authorization: `Bearer ${t}` } : {};
}

// ---------------------------------------------------------------------------
// Auto-refresh on 401 — called by `client.ts::request`.
// ---------------------------------------------------------------------------

/// Returns the new Session on success, or null on hard failure
/// (no session present, expired refresh, server error). Caller
/// is expected to retry their original request exactly once
/// when the result is non-null; on null they should drop the
/// session and prompt re-login.
export async function tryRefresh(): Promise<Session | null> {
  if (inflightRefresh) return inflightRefresh;
  inflightRefresh = (async () => {
    const cur = getSession();
    if (!cur) return null;
    if (Date.now() >= cur.refresh_expires_at) {
      setSession(null);
      return null;
    }
    try {
      const fresh = await authApi.refresh({ refresh_token: cur.refresh_token });
      const next = sessionFromTokenResponse(fresh);
      setSession(next);
      return next;
    } catch {
      setSession(null);
      return null;
    }
  })();
  try {
    return await inflightRefresh;
  } finally {
    inflightRefresh = null;
  }
}

// ---------------------------------------------------------------------------
// Cached auth-mode probe — populated by `loadAuthInfo()` at boot.
// ---------------------------------------------------------------------------

export function getAuthInfo(): AuthInfoResponse | null {
  return cachedAuthInfo;
}

export function getAuthMode(): AuthMode | null {
  return cachedAuthInfo?.mode ?? null;
}

export function onAuthInfoChange(fn: AuthInfoListener): () => void {
  authInfoListeners.add(fn);
  fn(cachedAuthInfo);
  return () => {
    authInfoListeners.delete(fn);
  };
}

/// Fetch + cache the public probe. Safe to call repeatedly;
/// only the most recent result is retained. On network error
/// the cache is cleared (so the UI falls back to its default
/// "show everything" posture rather than locking the user out
/// from a transient failure).
export async function loadAuthInfo(): Promise<AuthInfoResponse | null> {
  try {
    const info = await authApi.info();
    cachedAuthInfo = info;
    for (const fn of authInfoListeners) fn(info);
    return info;
  } catch {
    cachedAuthInfo = null;
    for (const fn of authInfoListeners) fn(null);
    return null;
  }
}

// ---------------------------------------------------------------------------
// Status pill — unchanged contract from Phase 0.
// ---------------------------------------------------------------------------

export function getAuthStatus(): AuthStatus {
  return status;
}

export function onAuthStatusChange(fn: StatusListener): () => void {
  statusListeners.add(fn);
  fn(status);
  return () => {
    statusListeners.delete(fn);
  };
}

/// Called by `client.ts::request()` after every fetch. Method
/// is the HTTP verb so we can distinguish gated writes from
/// anonymous GETs. A 2xx GET doesn't flip the pill green —
/// many engine GETs answer without auth and would give a false
/// positive that the credential is wired up.
export function reportRequestOutcome(method: string, httpStatus: number): void {
  if (httpStatus === 401 || httpStatus === 403) {
    publishStatus("unauthorized");
    return;
  }
  if (httpStatus >= 200 && httpStatus < 300) {
    const m = method.toUpperCase();
    if (m === "PUT" || m === "POST" || m === "DELETE" || m === "PATCH") {
      const haveCred = getSession() != null || getToken() != null;
      publishStatus(haveCred ? "ok" : "unknown");
    }
  }
}

function publishStatus(next: AuthStatus): void {
  if (next === status) return;
  status = next;
  for (const fn of statusListeners) fn(status);
}

// ---------------------------------------------------------------------------
// Top-level logout helper. Best-effort POST + always clear the
// local session.
// ---------------------------------------------------------------------------

export async function logout(): Promise<void> {
  const s = getSession();
  if (s) {
    try {
      await authApi.logout({ refresh_token: s.refresh_token }, s.access_token);
    } catch {
      // Network/engine failure — we still clear local state
      // below so the UI returns to the login overlay either
      // way. Worst case the refresh chain stays alive in the
      // DB until its expiry; not a security issue (the SPA no
      // longer has the secret).
    }
  }
  setSession(null);
}
