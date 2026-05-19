// M6 Phase 2 Step 2.9d — Playwright globalSetup for the
// local-mode auth e2e suite.
//
// Responsibilities (in order):
//
//   1. Make sure `target/debug/nexus-engine` exists. We do NOT
//      force the `test-injection` feature here (the auth suite
//      doesn't inject synthetic events — every assertion runs
//      against the REAL endpoints), but we DO build with
//      defaults so this works on first run after a `cargo
//      clean`.
//   2. Make sure `ui/dist` exists; run `npm run build` in `ui/`
//      if missing.
//   3. Pick one free port via a transient TCP bind.
//   4. Materialise a tempdir-rooted nexus.toml with
//      `auth.mode = "local"` AND a NULL `auth.admin_secret_path`
//      so the bootstrap fires on first boot and the engine
//      synthesises its own HS256 signing secret (mirrors the
//      "operator forgot to mint a secret" path the bootstrap
//      handles for them).
//   5. Spawn the engine with stderr captured to a buffer so we
//      can scrape the first-boot OTP from the bootstrap
//      `tracing::warn!` line. The line shape (from
//      `crates/nexus-engine/src/main.rs:212`) is
//      `... one_time_password=<otp> username=admin ...`. We
//      stop scraping after the first match.
//   6. Poll `GET /api/health` until 200 (or 30s).
//   7. Persist `{ baseURL, statedir, pid, bootstrapUsername,
//      bootstrapOtp }` to `ui/e2e/auth/.engine-state.json` so
//      per-spec fixtures + globalTeardown can read it.
//
// Differences from the M7 setup (`../../fixtures/global-setup.ts`):
//   * No mock webhook server — the auth suite doesn't dispatch.
//   * No `--mock-detector` — the suite doesn't care about frames.
//   * stderr capture is REQUIRED (the OTP only lives there).
//   * `auth.mode = "local"` instead of `"none"`.

import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import { createServer } from "node:net";
import { randomBytes } from "node:crypto";
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
// Repo root = ui/e2e/auth/fixtures → ../../../..
const REPO_ROOT = resolve(__dirname, "../../../..");
const ENGINE_BIN = join(REPO_ROOT, "target", "debug", "nexus-engine");
const UI_DIST = join(REPO_ROOT, "ui", "dist");
const SIDECAR = join(__dirname, "..", ".engine-state.json");

const BOOT_TIMEOUT_MS = 30_000;
const HEALTH_POLL_MS = 250;
// The bootstrap line is printed BEFORE the HTTP server binds,
// so capturing it during boot (rather than after) is mandatory.
const OTP_SCRAPE_TIMEOUT_MS = 20_000;

export default async function globalSetup(): Promise<void> {
  ensureEngineBinary();
  ensureUiDist();

  const port = await findFreePort();
  const baseURL = `http://127.0.0.1:${port}`;
  const statedir = mkdtempSync(join(tmpdir(), "nexus-e2e-auth-"));

  const tomlPath = writeEngineConfig(statedir, port);

  console.log(`[e2e:auth] booting engine at ${baseURL} (state=${statedir})`);
  const child = spawn(ENGINE_BIN, ["--config", tomlPath], {
    cwd: REPO_ROOT,
    env: {
      ...process.env,
      // We need `nexus_engine::auth=warn` (default level) to
      // see the bootstrap line. Don't quiet it.
      RUST_LOG: process.env["E2E_RUST_LOG"] ?? "warn,nexus=info",
      // Disable ANSI colour codes in tracing output — they
      // wrap the OTP value (`...=\x1b[1mvalue\x1b[0m`) and
      // break the regex scrape downstream. tracing_subscriber's
      // ansi layer honours NO_COLOR (see https://no-color.org/).
      NO_COLOR: "1",
    },
    // pipe stdout AND stderr so we can scrape the bootstrap
    // OTP. `tracing_subscriber::fmt::layer()` defaults to
    // stdout, so we must capture that pipe; stderr is also
    // wired in case logging is reconfigured later.
    // E2E_VERBOSE forces stdio inheritance for live debugging.
    stdio: process.env["E2E_VERBOSE"]
      ? "inherit"
      : ["ignore", "pipe", "pipe"],
    detached: false,
  });
  child.on("exit", (code, signal) => {
    if (code !== 0 && code !== null) {
      console.error(
        `[e2e:auth] engine exited unexpectedly code=${code} signal=${signal}`,
      );
    }
  });

  // Start scraping immediately — the bootstrap line fires
  // before health-poll succeeds, so concurrent scraping is
  // mandatory to avoid losing the OTP to a buffer drain.
  const otpPromise = scrapeBootstrapOtp(child);

  // Persist BEFORE health-poll so teardown can clean up even
  // if the engine never comes ready.
  writeFileSync(
    SIDECAR,
    JSON.stringify(
      { baseURL, statedir, pid: child.pid, bootstrapUsername: "admin", bootstrapOtp: null },
      null,
      2,
    ),
  );

  await waitForHealth(baseURL);
  const otp = await otpPromise;
  console.log(`[e2e:auth] engine ready in pid=${child.pid}; bootstrap user=admin`);

  // Re-write sidecar now that we have the OTP. Specs that need
  // to log in as the bootstrap admin read this file.
  writeFileSync(
    SIDECAR,
    JSON.stringify(
      {
        baseURL,
        statedir,
        pid: child.pid,
        bootstrapUsername: "admin",
        bootstrapOtp: otp,
      },
      null,
      2,
    ),
  );

  process.env["E2E_BASE_URL"] = baseURL;
  process.env["E2E_BOOTSTRAP_USERNAME"] = "admin";
  process.env["E2E_BOOTSTRAP_OTP"] = otp;
}

function ensureEngineBinary(): void {
  console.log("[e2e:auth] ensuring engine binary built");
  const r = spawnSync("cargo", ["build", "-p", "nexus-engine"], {
    cwd: REPO_ROOT,
    stdio: "inherit",
  });
  if (r.status !== 0) {
    throw new Error("cargo build failed; cannot run e2e:auth");
  }
  if (!existsSync(ENGINE_BIN)) {
    throw new Error(`cargo build succeeded but ${ENGINE_BIN} is missing`);
  }
}

function ensureUiDist(): void {
  if (existsSync(join(UI_DIST, "index.html"))) return;
  console.log("[e2e:auth] ui/dist missing — running `npm run build` in ui/");
  const r = spawnSync("npm", ["run", "build"], {
    cwd: join(REPO_ROOT, "ui"),
    stdio: "inherit",
  });
  if (r.status !== 0) {
    throw new Error("ui build failed; cannot run e2e:auth");
  }
}

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

function writeEngineConfig(statedir: string, port: number): string {
  const dataDir = join(statedir, "data");
  const clipsDir = join(dataDir, "clips");
  const stateDir = join(dataDir, "state");
  const uiRoot = UI_DIST;
  const dbPath = join(dataDir, "nexus.db");
  const adminSecretPath = join(stateDir, "admin-secret");
  mkdirSync(dataDir, { recursive: true });
  mkdirSync(clipsDir, { recursive: true });
  mkdirSync(stateDir, { recursive: true });
  // Pre-mint the HS256 signing secret used to mint local-mode
  // session JWTs. Engine refuses to sign on
  // `POST /api/v1/auth/login` with 503 `auth_not_configured`
  // when `auth.admin_secret_path` is unset, so we MUST provide
  // one. A 32-byte cryptographically random string is what the
  // operator docs recommend.
  writeFileSync(adminSecretPath, randomBytes(32).toString("hex"), {
    mode: 0o600,
  });

  // Differences from the M7 setup config:
  //   * `auth.mode = "local"` triggers bootstrap on first boot.
  //   * `auth.admin_secret_path` points at a freshly-minted
  //     32-byte hex secret under the state dir.
  //   * No `[[sinks]]`, no `[[cameras]]` — this suite tests
  //     auth and admin/users only. A virtual camera spinning
  //     in the background just slows boot.
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
mode = "local"
admin_secret_path = "${adminSecretPath}"

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

[bus]
backend = "broadcast"
capacity = 256
`;
  const path = join(statedir, "nexus.toml");
  writeFileSync(path, toml);
  return path;
}

/// Scrape the engine's stdout/stderr for the bootstrap `warn!` line.
///
/// The line is emitted by `main.rs` immediately after
/// `bootstrap_if_needed` returns `AdminCreated`, with the
/// `tracing` default text formatter — so it contains the
/// substring `one_time_password=<value>`. We pull the value
/// out via a regex that stops at the next whitespace.
///
/// `tracing_subscriber::fmt::layer()` defaults to **stdout**,
/// so we wire both pipes to be safe (and so JSON-mode logs
/// would also be caught if the config ever switches).
function scrapeBootstrapOtp(child: ChildProcess): Promise<string> {
  return new Promise((resolveP, rejectP) => {
    const deadline = setTimeout(() => {
      cleanup();
      rejectP(
        new Error(
          `bootstrap OTP not seen within ${OTP_SCRAPE_TIMEOUT_MS}ms`,
        ),
      );
    }, OTP_SCRAPE_TIMEOUT_MS);

    let buffer = "";
    let resolved = false;

    function onData(chunk: Buffer | string): void {
      buffer += typeof chunk === "string" ? chunk : chunk.toString("utf8");
      // Match the field as tracing renders it: `one_time_password=<...>`.
      // The value is URL-safe-base64-no-pad: `[A-Za-z0-9_-]+`.
      const m = /one_time_password=([A-Za-z0-9_-]+)/.exec(buffer);
      if (m && m[1]) {
        resolved = true;
        cleanup();
        resolveP(m[1]);
      }
    }

    function cleanup(): void {
      clearTimeout(deadline);
      if (child.stdout) child.stdout.off("data", onData);
      if (child.stderr) child.stderr.off("data", onData);
    }

    if (!child.stdout && !child.stderr) {
      cleanup();
      rejectP(new Error("engine child has neither stdout nor stderr pipe"));
      return;
    }
    if (child.stdout) child.stdout.on("data", onData);
    if (child.stderr) child.stderr.on("data", onData);

    child.on("exit", (code, signal) => {
      if (!resolved) {
        cleanup();
        rejectP(
          new Error(
            `engine exited before OTP could be scraped (code=${code} signal=${signal})`,
          ),
        );
      }
    });
  });
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
  throw new Error(
    `engine never reached /api/health within ${BOOT_TIMEOUT_MS}ms: ${String(lastErr)}`,
  );
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
