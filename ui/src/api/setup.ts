// First-boot setup wizard endpoints. Engine contract:
//   GET  /api/v1/setup/status   → SetupStatus (auth required: SessionContext)
//   POST /api/v1/setup/complete → 204         (auth required: AdminContext)
//
// `setup_complete` is a one-way latch persisted in
// `engine_runtime_settings.setup_complete`. The SPA router uses it to
// decide whether to bounce the user to `/setup` after login.

import { api } from "@/api/client";

export interface SetupStatus {
  setup_complete: boolean;
  cameras_count: number;
  rules_count: number;
  admin_count: number;
  version: string;
  hostname: string;
  /** True when the logged-in user is still using the bootstrap OTP and
   *  MUST change it before the wizard can advance past the password step. */
  session_force_password_reset: boolean;
}

export const setupApi = {
  status: () => api.get<SetupStatus>("/v1/setup/status"),
  complete: () => api.post<void>("/v1/setup/complete", {}),
};
