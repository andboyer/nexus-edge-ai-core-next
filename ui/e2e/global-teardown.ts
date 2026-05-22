// Global teardown: SIGTERM the engine spawned in global-setup, clean up the
// scratch workdir. Idempotent.

import { readFileSync, unlinkSync } from "node:fs";

import { SIDECAR, cleanupWorkdir } from "./global-setup";

export default async function globalTeardown() {
  if (process.env.E2E_SKIP_ENGINE === "1") return;

  let sidecar:
    | { pid: number; baseUrl: string; adminOtp: string; workDir?: string }
    | null = null;
  try {
    sidecar = JSON.parse(readFileSync(SIDECAR, "utf8"));
  } catch {
    return;
  }
  if (!sidecar) return;

  if (sidecar.pid > 0) {
    try {
      process.kill(sidecar.pid, "SIGTERM");
    } catch {
      // already dead
    }
    // Give it a moment to flush.
    await new Promise((res) => setTimeout(res, 500));
    try {
      process.kill(sidecar.pid, "SIGKILL");
    } catch {
      // already dead
    }
  }

  cleanupWorkdir(sidecar.workDir);

  try {
    unlinkSync(SIDECAR);
  } catch {
    // ignore
  }
}
