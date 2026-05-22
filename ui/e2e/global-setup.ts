// Global setup: write a minimal config, mint an admin secret, spawn the
// engine, capture the bootstrap one_time_password from stdout, and write
// a sidecar JSON for specs (pid, base URL, admin OTP, workDir).
//
// The same lessons from user memory apply:
//   - `NO_COLOR=1` to strip ANSI from tracing output so the regex matches.
//   - Pipe stdout (tracing writes there), not just stderr.
//   - `auth.mode = "local"` needs `auth.admin_secret_path` (32-byte file,
//     mode 0600) configured before boot.
//   - Bootstrap line is `tracing::warn!(... one_time_password = %otp, ...)` so
//     the field renders as `one_time_password=<OTP>` in non-color logs.

import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { writeFileSync, mkdirSync, rmSync, existsSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";


const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = join(HERE, "..", "..");

const SIDECAR = join(tmpdir(), "nexus-e2e-sidecar.json");

async function pickFreePort(): Promise<number> {
  return new Promise<number>((resolve, reject) => {
    const srv = createServer();
    srv.on("error", reject);
    srv.listen(0, "127.0.0.1", () => {
      const addr = srv.address();
      if (addr && typeof addr === "object") {
        const port = addr.port;
        srv.close(() => resolve(port));
      } else {
        reject(new Error("could not pick port"));
      }
    });
  });
}

// Playwright passes its FullConfig but we don't consume it (port/log paths
// are driven by env vars + SIDECAR). Declared without args to satisfy
// @typescript-eslint/no-unused-vars; Playwright's globalSetup signature
// permits a zero-arg function.
export default async function globalSetup() {
  if (process.env.E2E_SKIP_ENGINE === "1") {
    writeFileSync(
      SIDECAR,
      JSON.stringify({
        pid: -1,
        baseUrl: process.env.E2E_BASE_URL ?? "http://127.0.0.1:8089",
        adminOtp: process.env.E2E_ADMIN_OTP ?? "",
      }),
    );
    return;
  }

  const distDir = join(REPO_ROOT, "ui", "dist");
  if (!existsSync(distDir)) {
    throw new Error(
      `ui/dist not found at ${distDir} — run \`npm run build\` in ui/ first`,
    );
  }

  const enginePath = join(REPO_ROOT, "target", "debug", "nexus-engine");
  if (!existsSync(enginePath)) {
    throw new Error(
      `nexus-engine binary not found at ${enginePath} — run \`cargo build -p nexus-engine\` first`,
    );
  }

  const port = Number(process.env.E2E_PORT ?? (await pickFreePort()));
  const baseUrl = `http://127.0.0.1:${port}`;

  // Workspace per run so consecutive runs don't share state.
  const workDir = join(tmpdir(), `nexus-e2e-${Date.now()}`);
  mkdirSync(workDir, { recursive: true });

  const secretPath = join(workDir, "admin_secret");
  // Engine reads this as UTF-8 text (or JSON `{"secret":"..."}`). 32 bytes
  // of randomness as hex → 64 chars, well above any min-length check.
  writeFileSync(secretPath, randomBytes(32).toString("hex"), {
    mode: 0o600,
  });

  const stateDir = join(workDir, "state");
  const clipsDir = join(workDir, "clips");
  const dbPath = join(workDir, "nexus.db");
  mkdirSync(stateDir, { recursive: true });
  mkdirSync(clipsDir, { recursive: true });

  // Config schema matches crates/nexus-config (deny_unknown_fields). See
  // config/single-camera.toml for the reference layout.
  const configPath = join(workDir, "nexus.toml");
  writeFileSync(
    configPath,
    [
      `[runtime]`,
      `state_dir = "${stateDir}"`,
      ``,
      `[runtime.clips]`,
      `clips_dir = "${clipsDir}"`,
      ``,
      `[server]`,
      `api_bind = "127.0.0.1:${port}"`,
      `ui_root = "${distDir}"`,
      ``,
      `[store]`,
      `url = "sqlite:${dbPath}?mode=rwc"`,
      `seed_from_config = true`,
      ``,
      `[telemetry]`,
      `log_level = "warn,nexus_engine=info"`,
      ``,
      `[auth]`,
      `mode = "local"`,
      `admin_secret_path = "${secretPath}"`,
      ``,
      `[inference]`,
      `backend = "in_process"`,
      `workers = 1`,
      ``,
      `[inference.model]`,
      `kind = "mock"`,
      `input_width = 640`,
      `input_height = 480`,
      ``,
      `[tracker]`,
      `backend = "iou_naive"`,
      ``,
      `[rules]`,
      `backend = "cel"`,
      ``,
      `[bus]`,
      `backend = "broadcast"`,
      `capacity = 256`,
      ``,
      // No [[cameras]]: fresh DB lets cameras.spec assert the empty state.
      // Specs that need a camera should create one via the UI or the API.
      ``,
    ].join("\n"),
  );

  const child = spawn(
    enginePath,
    ["--config", configPath, "--mock-detector"],
    {
      env: {
        ...process.env,
        NO_COLOR: "1",
        RUST_LOG: process.env.RUST_LOG ?? "warn,nexus_engine=info",
      },
      stdio: ["ignore", "pipe", "pipe"],
    },
  );

  let adminOtp = "";
  let stdoutBuf = "";
  let stderrBuf = "";
  // Bootstrap line emits via `tracing::warn!(... one_time_password = %otp, ...)`.
  // OTP is URL_SAFE_NO_PAD base64 (chars: A-Z, a-z, 0-9, _, -), ~43 chars for 32 bytes.
  const otpRegex = /one_time_password[=:]\s*([A-Za-z0-9_-]{16,})/i;

  const scrape = (chunk: Buffer, isStderr: boolean) => {
    const s = chunk.toString();
    if (isStderr) stderrBuf += s;
    else stdoutBuf += s;
    if (!adminOtp) {
      const m = otpRegex.exec(stdoutBuf + stderrBuf);
      if (m && m[1]) adminOtp = m[1];
    }
    if (process.env.E2E_VERBOSE === "1") {
      process.stderr.write(s);
    }
  };

  child.stdout.on("data", (c: Buffer) => scrape(c, false));
  child.stderr.on("data", (c: Buffer) => scrape(c, true));

  child.on("exit", (code, signal) => {
    if (process.env.E2E_VERBOSE === "1") {
      process.stderr.write(
        `[engine] exit code=${code} signal=${signal}\n`,
      );
    }
  });

  // Wait for /api/health to respond, up to 30s.
  const deadline = Date.now() + 30_000;
  let healthy = false;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) break;
    try {
      const r = await fetch(`${baseUrl}/api/health`);
      if (r.ok) {
        healthy = true;
        break;
      }
    } catch {
      // not yet ready
    }
    await new Promise((res) => setTimeout(res, 250));
  }

  if (!healthy) {
    try {
      child.kill("SIGTERM");
    } catch {
      // ignore
    }
    throw new Error(
      `engine did not become healthy within 30s\n` +
        `--- stdout ---\n${stdoutBuf}\n` +
        `--- stderr ---\n${stderrBuf}\n`,
    );
  }

  if (!adminOtp) {
    // Engine is healthy but we never matched the OTP. Likely the regex
    // missed; surface the captured logs so it's obvious why.
    throw new Error(
      `engine healthy but no admin OTP captured\n` +
        `--- stdout ---\n${stdoutBuf}\n` +
        `--- stderr ---\n${stderrBuf}\n`,
    );
  }

  writeFileSync(
    SIDECAR,
    JSON.stringify({
      pid: child.pid,
      baseUrl,
      adminOtp,
      workDir,
    }),
  );

  child.unref();
}

export { SIDECAR };

export function cleanupWorkdir(workDir: string | undefined) {
  if (!workDir) return;
  try {
    rmSync(workDir, { recursive: true, force: true });
  } catch {
    // ignore
  }
}
