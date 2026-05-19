// M7 Step 6F + 6F2 — Playwright globalSetup.
//
// Responsibilities (in order):
//   1. Make sure `target/debug/nexus-engine` exists *with the
//      `test-injection` feature compiled in*. If not, run
//      `cargo build -p nexus-engine --features test-injection`
//      inline so first-time runs of `npm run e2e` work end-to-end.
//      Inherited stdio so the operator sees compile progress.
//   2. Make sure `ui/dist` exists (the engine serves the SPA from
//      cfg.server.ui_root). Run `vite build` if missing.
//   3. Pick two free ports via transient TCP binds: one for the
//      engine, one for the mock webhook server. Loopback bind
//      keeps everything off the public interface.
//   4. Spawn the mock webhook server (separate Node process) so
//      the dispatcher has somewhere to POST happy-path payloads
//      and the suppression specs can assert `count === 0`.
//   5. Materialise a tempdir-rooted nexus.toml derived from the
//      smoke-test config (mock detector, virtual camera, sqlite
//      file under the tempdir, auth.mode = "none" on loopback,
//      single `[[sinks]]` entry pointing at the mock URL).
//   6. Spawn the engine with `--config <toml> --mock-detector`,
//      poll `GET /api/health` until 200 (or 30s).
//   7. Persist `{ baseURL, statedir, pid, mockUrl, mockPid }` to
//      `e2e/.engine-state.json` so per-spec fixtures + globalTeardown
//      can read it without needing module state.
//
// We deliberately do NOT use Playwright's `webServer` block — the
// boot probe (health-poll + readiness) is more involved than
// `webServer.command` supports cleanly, and we need teardown to
// rm the tempdir AFTER the engine releases SQLite WAL handles.

import { spawn, spawnSync } from "node:child_process";
import { createServer } from "node:net";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
// Repo root = ui/e2e/fixtures → ../../..
const REPO_ROOT = resolve(__dirname, "../../..");
const ENGINE_BIN = join(REPO_ROOT, "target", "debug", "nexus-engine");
const UI_DIST = join(REPO_ROOT, "ui", "dist");
const MOCK_WEBHOOK_SCRIPT = join(__dirname, "mock-webhook-server.mjs");
const SIDECAR = join(__dirname, "..", ".engine-state.json");

const BOOT_TIMEOUT_MS = 30_000;
const HEALTH_POLL_MS = 250;
const MOCK_READY_TIMEOUT_MS = 5_000;

export default async function globalSetup(): Promise<void> {
  ensureEngineBinary();
  ensureUiDist();

  // Two free ports: one for the engine, one for the mock
  // webhook server. Pick them in sequence so the close+rebind
  // race window is small and they can never collide with each other.
  const port = await findFreePort();
  const mockPort = await findFreePort();
  const baseURL = `http://127.0.0.1:${port}`;
  const mockUrl = `http://127.0.0.1:${mockPort}`;
  const statedir = mkdtempSync(join(tmpdir(), "nexus-e2e-"));

  // Spawn the mock webhook server FIRST so the engine has
  // something to dial when the dispatcher runs its first tick.
  // (Engine config pins `mockUrl/webhook` as the sink target.)
  const mockChild = spawn("node", [MOCK_WEBHOOK_SCRIPT, String(mockPort)], {
    cwd: REPO_ROOT,
    env: { ...process.env },
    stdio: process.env["E2E_VERBOSE"] ? "inherit" : ["ignore", "pipe", "pipe"],
    detached: false,
  });
  mockChild.on("exit", (code, signal) => {
    if (code !== 0 && code !== null) {
      console.error(`[e2e] mock-webhook exited unexpectedly code=${code} signal=${signal}`);
    }
  });
  await waitForMockReady(mockUrl);
  console.log(`[e2e] mock webhook ready at ${mockUrl} (pid=${mockChild.pid})`);

  const tomlPath = writeEngineConfig(statedir, port, mockUrl);

  console.log(`[e2e] booting engine at ${baseURL} (state=${statedir})`);
  const child = spawn(ENGINE_BIN, ["--config", tomlPath, "--mock-detector"], {
    cwd: REPO_ROOT,
    env: {
      ...process.env,
      RUST_LOG: process.env["E2E_RUST_LOG"] ?? "warn,nexus=info",
    },
    stdio: process.env["E2E_VERBOSE"] ? "inherit" : "ignore",
    detached: false,
  });
  child.on("exit", (code, signal) => {
    // If the engine dies before teardown, surface it loudly so the
    // test failure isn't just "ECONNREFUSED" on every request.
    if (code !== 0 && code !== null) {
      console.error(`[e2e] engine exited unexpectedly code=${code} signal=${signal}`);
    }
  });

  // Persist before health-poll so teardown can clean up even if
  // the engine never comes ready.
  writeFileSync(
    SIDECAR,
    JSON.stringify(
      { baseURL, statedir, pid: child.pid, mockUrl, mockPid: mockChild.pid },
      null,
      2,
    ),
  );

  await waitForHealth(baseURL);
  console.log(`[e2e] engine ready in pid=${child.pid}`);

  // Make baseURL + mockUrl available to specs via env (Playwright's
  // config reads E2E_BASE_URL; specs read E2E_MOCK_URL directly).
  process.env["E2E_BASE_URL"] = baseURL;
  process.env["E2E_MOCK_URL"] = mockUrl;
}

function ensureEngineBinary(): void {
  // We always rebuild with `--features test-injection` because the
  // e2e suite needs the `_test/inject_event` endpoint. Cargo is
  // smart enough to no-op when the features-resolved fingerprint
  // hasn't changed, so this is fast on the warm path.
  console.log("[e2e] ensuring engine binary built with --features test-injection");
  const r = spawnSync(
    "cargo",
    ["build", "-p", "nexus-engine", "--features", "test-injection"],
    { cwd: REPO_ROOT, stdio: "inherit" },
  );
  if (r.status !== 0) {
    throw new Error("cargo build failed; cannot run e2e");
  }
  if (!existsSync(ENGINE_BIN)) {
    throw new Error(`cargo build succeeded but ${ENGINE_BIN} is missing`);
  }
}

function ensureUiDist(): void {
  if (existsSync(join(UI_DIST, "index.html"))) return;
  console.log("[e2e] ui/dist missing — running `npm run build` in ui/");
  const r = spawnSync("npm", ["run", "build"], {
    cwd: join(REPO_ROOT, "ui"),
    stdio: "inherit",
  });
  if (r.status !== 0) {
    throw new Error("ui build failed; cannot run e2e");
  }
}

// Bind to :0, read the OS-assigned port, close. Standard
// "find a free port" trick; the port is briefly available between
// close() and the engine's bind, but on a loopback test box the
// race is statistically irrelevant.
function findFreePort(): Promise<number> {
  return new Promise((resolveP, rejectP) => {
    const srv = createServer();
    srv.unref();
    srv.on("error", rejectP);
    srv.listen(0, "127.0.0.1", () => {
      const addr = srv.address();
      if (addr && typeof addr === "object") {
        const port = addr.port;
        srv.close(() => resolveP(port));
      } else {
        rejectP(new Error("no address from net.createServer"));
      }
    });
  });
}

function writeEngineConfig(statedir: string, port: number, mockUrl: string): string {
  const dataDir = join(statedir, "data");
  const clipsDir = join(dataDir, "clips");
  const stateDir = join(dataDir, "state");
  const uiRoot = UI_DIST;
  const dbPath = join(dataDir, "nexus.db");
  mkdirSync(dataDir, { recursive: true });
  mkdirSync(clipsDir, { recursive: true });
  mkdirSync(stateDir, { recursive: true });

  // Derived from config/single-camera.toml. Differences:
  //   - all paths rooted under the per-run tempdir
  //   - api_bind uses the OS-assigned free port
  //   - auth.mode = "none" + loopback bind keeps the admin gate
  //     happy without us having to mint a JWT
  //   - one always-firing rule so the rules-list spec has
  //     something to render
  //   - one webhook sink pointed at the mock-webhook server so
  //     6F2 cascade/happy-path specs can observe dispatch behaviour
  const toml = `[runtime]
state_dir = "${stateDir}"

[runtime.clips]
clips_dir = "${clipsDir}"

[server]
api_bind = "127.0.0.1:${port}"
ui_root = "${uiRoot}"

[store]
url = "sqlite:${dbPath}?mode=rwc"
seed_from_config = true

[telemetry]
log_level = "warn,nexus=info"

[auth]
mode = "none"

[inference]
backend = "in_process"
workers = 1

[inference.model]
kind = "mock"
input_width = 640
input_height = 480

[tracker]
backend = "iou_naive"

[rules]
backend = "cel"

[[rules.inline]]
id = "any_person"
name = "Any person (e2e seed)"
when = "object.label == 'person'"
severity = "low"
min_track_age_ms = 0
consecutive_frames = 1
cooldown_ms = 5000
enabled = true

[bus]
backend = "broadcast"
capacity = 256

[[cameras]]
id = 1
name = "Virtual (e2e)"
url = "virtual://local"
enabled = true
prompts = ["person"]
max_fps = 5

[[sinks]]
kind = "webhook"
name = "e2e"
url = "${mockUrl}/webhook"
timeout_secs = 5
`;
  const path = join(statedir, "nexus.toml");
  writeFileSync(path, toml);
  return path;
}

async function waitForHealth(baseURL: string): Promise<void> {
  const deadline = Date.now() + BOOT_TIMEOUT_MS;
  let lastErr: unknown = null;
  while (Date.now() < deadline) {
    try {
      const r = await fetch(`${baseURL}/api/health`);
      if (r.ok) return;
      lastErr = new Error(`health returned ${r.status}`);
    } catch (e) {
      lastErr = e;
    }
    await sleep(HEALTH_POLL_MS);
  }
  throw new Error(`engine never reached /api/health within ${BOOT_TIMEOUT_MS}ms: ${String(lastErr)}`);
}

async function waitForMockReady(mockUrl: string): Promise<void> {
  const deadline = Date.now() + MOCK_READY_TIMEOUT_MS;
  let lastErr: unknown = null;
  while (Date.now() < deadline) {
    try {
      const r = await fetch(`${mockUrl}/_count`);
      if (r.ok) return;
      lastErr = new Error(`mock /_count returned ${r.status}`);
    } catch (e) {
      lastErr = e;
    }
    await sleep(100);
  }
  throw new Error(`mock webhook never came ready within ${MOCK_READY_TIMEOUT_MS}ms: ${String(lastErr)}`);
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
