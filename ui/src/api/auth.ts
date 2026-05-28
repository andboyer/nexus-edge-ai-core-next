// Auth endpoint wrappers. Engine contract:
//   GET    /api/v1/auth/info          \u2192 AuthInfo (public)
//   POST   /api/v1/auth/login         \u2192 TokenResponse
//   POST   /api/v1/auth/refresh       \u2192 TokenResponse
//   POST   /api/v1/auth/logout        \u2192 204
//   POST   /api/v1/auth/change-password \u2192 204 (NO BODY \u2014 do not parse)
//   POST   /api/v1/auth/oidc/start    \u2192 { authorization_url, state }
//
// The 204 for change-password is critical: the engine rotates the
// server-side refresh chain but leaves the existing access_token
// valid until TTL. Returning Promise<void> here keeps callers honest
// (no accidental `await ... .access_token`).

import { api } from "@/api/client";
import type { AuthInfo, TokenResponse } from "@/api/types";

export interface OidcStartResponse {
  authorization_url: string;
  state: string;
}

export const authApi = {
  info: () => api.get<AuthInfo>("/v1/auth/info"),

  login: (username: string, password: string) =>
    api.post<TokenResponse>("/v1/auth/login", { username, password }),

  refresh: (refresh_token: string) =>
    api.post<TokenResponse>("/v1/auth/refresh", { refresh_token }),

  logout: (refresh_token?: string) =>
    api.post<void>("/v1/auth/logout", refresh_token ? { refresh_token } : {}),

  changePassword: (old_password: string, new_password: string) =>
    api.post<void>("/v1/auth/change-password", { old_password, new_password }),

  // First-run setup. Unauthenticated. Only call when
  // `info.first_run_pending` is true; the engine returns 409
  // otherwise. On success returns the same TokenResponse shape
  // as /auth/login, so the caller can sign the operator in
  // immediately.
  firstRunSetup: (password: string, username?: string) =>
    api.post<TokenResponse>(
      "/v1/auth/first-run-setup",
      username ? { username, password } : { password },
    ),

  // OIDC: mints PKCE/state/nonce server-side, returns the IdP
  // authorization URL. Caller assigns it to `window.location` to
  // hand control to the IdP. The state cookie is set automatically
  // by the response (HttpOnly, scoped to /api/v1/auth).
  oidcStart: (redirect_to?: string) =>
    api.post<OidcStartResponse>(
      "/v1/auth/oidc/start",
      redirect_to ? { redirect_to } : {},
    ),
};
