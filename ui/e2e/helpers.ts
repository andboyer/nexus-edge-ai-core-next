// Shared spec helpers.

import { readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import type { Page } from "@playwright/test";

const SIDECAR = join(tmpdir(), "nexus-e2e-sidecar.json");

export interface Sidecar {
  pid: number;
  baseUrl: string;
  adminOtp: string;
  workDir?: string;
}

export function readSidecar(): Sidecar {
  return JSON.parse(readFileSync(SIDECAR, "utf8")) as Sidecar;
}

/**
 * Mint a fresh admin session by hitting the bootstrap OTP login flow.
 * Sets the session in localStorage so the SPA's `_app` route gate passes.
 */
export async function loginAsAdmin(page: Page): Promise<void> {
  const sidecar = readSidecar();
  if (!sidecar.adminOtp) {
    throw new Error(
      "no admin OTP captured during globalSetup — engine did not emit a bootstrap OTP, " +
        "or the regex in global-setup.ts failed to match. Try RUST_LOG=info E2E_VERBOSE=1.",
    );
  }

  // Hit /api/v1/auth/login directly to get a token pair, then plant the
  // session in localStorage in the page context. This avoids depending on
  // the visual login form for non-auth specs.
  const res = await fetch(`${sidecar.baseUrl}/api/v1/auth/login`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ username: "admin", password: sidecar.adminOtp }),
  });
  if (!res.ok) {
    throw new Error(
      `bootstrap login failed: HTTP ${res.status} ${await res.text()}`,
    );
  }
  const tokens = (await res.json()) as {
    access_token: string;
    refresh_token: string;
    expires_in: number;
    refresh_expires_in: number;
    user: { id: string; username: string; role: string };
  };

  // Plant the session in localStorage before the first nav. We have to use
  // addInitScript so it's set before any `_app` beforeLoad gate runs.
  await page.addInitScript((session) => {
    localStorage.setItem("nexus_session", JSON.stringify(session));
  }, {
    access_token: tokens.access_token,
    refresh_token: tokens.refresh_token,
    access_expires_at: Date.now() + tokens.expires_in * 1000,
    refresh_expires_at: Date.now() + tokens.refresh_expires_in * 1000,
    user: tokens.user,
  });
}
