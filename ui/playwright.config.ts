// M7 Step 6F — Playwright e2e config.
//
// We run against the *real* engine binary serving the *built* UI
// (vite build output) from a single port. This avoids the vite-dev
// proxy entirely — the production wire path is what gets exercised.
//
// Lifecycle:
//   * globalSetup.ts boots `target/debug/nexus-engine` against a
//     tempdir + free port, writes `ui/e2e/.engine-state.json` with
//     { baseURL, statedir, pid } for tests + teardown to read.
//   * globalTeardown.ts SIGTERMs the engine and rms the tempdir.
//
// Tests read `process.env.E2E_BASE_URL` (set by globalSetup) for the
// engine URL. Workers are pinned to 1 because the global-setup engine
// is shared state — parallel writes would race on `delivery_settings`.

import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./e2e",
  testIgnore: ["**/fixtures/**", "**/.engine-state.json"],
  // The shared engine is single-state; parallel workers would race on
  // /admin/delivery writes. 6F's whole point is the cascade — a single
  // worker that runs specs sequentially is exactly right.
  workers: 1,
  fullyParallel: false,
  retries: process.env["CI"] ? 1 : 0,
  reporter: process.env["CI"] ? [["github"], ["html", { open: "never" }]] : "list",
  timeout: 30_000,
  expect: {
    timeout: 10_000,
  },
  use: {
    baseURL: process.env["E2E_BASE_URL"] ?? "http://127.0.0.1:18089",
    headless: true,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
  },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
  globalSetup: "./e2e/fixtures/global-setup.ts",
  globalTeardown: "./e2e/fixtures/global-teardown.ts",
});
