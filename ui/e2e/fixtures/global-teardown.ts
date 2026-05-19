// M7 Step 6F — Playwright globalTeardown.
//
// Reads the sidecar that globalSetup wrote, SIGTERMs the engine,
// gives it 5s to release SQLite WAL handles, then rms the tempdir.
// Best-effort: a teardown failure must not mask a real spec failure,
// so every step swallows its own errors with a log line.

import { existsSync, readFileSync, rmSync, unlinkSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const SIDECAR = join(__dirname, "..", ".engine-state.json");

interface EngineState {
  baseURL: string;
  statedir: string;
  pid: number | null;
  // M7 Step 6F2 — mock webhook server pid + URL. Optional so an
  // older sidecar (from a partial-run leftover) doesn't trip the
  // teardown path.
  mockUrl?: string;
  mockPid?: number | null;
}

export default async function globalTeardown(): Promise<void> {
  if (!existsSync(SIDECAR)) {
    console.warn("[e2e] teardown: no sidecar (.engine-state.json); nothing to clean up");
    return;
  }

  let state: EngineState;
  try {
    state = JSON.parse(readFileSync(SIDECAR, "utf8")) as EngineState;
  } catch (e) {
    console.warn(`[e2e] teardown: could not parse sidecar: ${String(e)}`);
    return;
  }

  if (state.pid && state.pid > 0) {
    try {
      process.kill(state.pid, "SIGTERM");
      // Engine writes the SQLite WAL on shutdown; 5s is generous
      // for the empty-DB case but cheap to wait.
      await sleep(500);
      // If it's still alive after the grace period, escalate.
      if (isAlive(state.pid)) {
        await sleep(4500);
        if (isAlive(state.pid)) {
          process.kill(state.pid, "SIGKILL");
        }
      }
    } catch (e) {
      // ESRCH = already gone, ignore. Anything else: log + continue.
      const msg = String((e as NodeJS.ErrnoException).code ?? e);
      if (msg !== "ESRCH") {
        console.warn(`[e2e] teardown: kill pid=${state.pid} failed: ${msg}`);
      }
    }
  }

  // Mock webhook server — same shape, much shorter grace because
  // there's nothing on disk to flush.
  if (state.mockPid && state.mockPid > 0) {
    try {
      process.kill(state.mockPid, "SIGTERM");
      await sleep(200);
      if (isAlive(state.mockPid)) {
        await sleep(1800);
        if (isAlive(state.mockPid)) {
          process.kill(state.mockPid, "SIGKILL");
        }
      }
    } catch (e) {
      const msg = String((e as NodeJS.ErrnoException).code ?? e);
      if (msg !== "ESRCH") {
        console.warn(`[e2e] teardown: kill mockPid=${state.mockPid} failed: ${msg}`);
      }
    }
  }

  try {
    rmSync(state.statedir, { recursive: true, force: true });
  } catch (e) {
    console.warn(`[e2e] teardown: rm ${state.statedir} failed: ${String(e)}`);
  }

  try {
    unlinkSync(SIDECAR);
  } catch {
    // Not fatal.
  }
}

function isAlive(pid: number): boolean {
  try {
    // Signal 0 doesn't deliver a signal — just probes existence.
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
