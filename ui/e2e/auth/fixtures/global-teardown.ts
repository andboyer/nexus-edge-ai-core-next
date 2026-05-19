// M6 Phase 2 Step 2.9d — Playwright globalTeardown for the
// local-mode auth e2e suite.
//
// Same shape as the M7 teardown: SIGTERM the engine, give it
// 5s to flush SQLite WAL handles, then rm the tempdir.
// Best-effort: every step swallows its own errors with a log
// line so a teardown failure can't mask a real spec failure.

import { existsSync, readFileSync, rmSync, unlinkSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const SIDECAR = join(__dirname, "..", ".engine-state.json");

interface EngineState {
  baseURL: string;
  statedir: string;
  pid: number | null;
  bootstrapUsername: string;
  bootstrapOtp: string | null;
}

export default async function globalTeardown(): Promise<void> {
  if (!existsSync(SIDECAR)) {
    console.warn(
      "[e2e:auth] teardown: no sidecar (.engine-state.json); nothing to clean up",
    );
    return;
  }

  let state: EngineState;
  try {
    state = JSON.parse(readFileSync(SIDECAR, "utf8")) as EngineState;
  } catch (e) {
    console.warn(`[e2e:auth] teardown: could not parse sidecar: ${String(e)}`);
    return;
  }

  if (state.pid && state.pid > 0) {
    try {
      process.kill(state.pid, "SIGTERM");
      await sleep(500);
      if (isAlive(state.pid)) {
        await sleep(4500);
        if (isAlive(state.pid)) {
          process.kill(state.pid, "SIGKILL");
        }
      }
    } catch (e) {
      const msg = String((e as NodeJS.ErrnoException).code ?? e);
      if (msg !== "ESRCH") {
        console.warn(
          `[e2e:auth] teardown: kill pid=${state.pid} failed: ${msg}`,
        );
      }
    }
  }

  try {
    rmSync(state.statedir, { recursive: true, force: true });
  } catch (e) {
    console.warn(
      `[e2e:auth] teardown: rm ${state.statedir} failed: ${String(e)}`,
    );
  }

  try {
    unlinkSync(SIDECAR);
  } catch {
    // Not fatal.
  }
}

function isAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
