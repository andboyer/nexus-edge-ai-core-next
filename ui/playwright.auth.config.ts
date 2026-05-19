// M6 Phase 2 Step 2.9d — Playwright e2e config for the local-mode
// auth + admin/users CRUD round-trip.
//
// Lives next to the M7 `playwright.config.ts` because the two
// suites need DIFFERENT engines: M7 e2e runs `auth.mode = "none"`
// for permissive admin access; this suite runs
// `auth.mode = "local"` so we can exercise the login overlay +
// force-password-reset modal + admin user CRUD. Spinning up
// both engines in one config would race on database state and
// hide cross-suite contamination.
//
// Run with `npm run e2e:auth` from the ui/ directory.
//
// Lifecycle (mirrors playwright.config.ts):
//   * globalSetup boots `target/debug/nexus-engine` with
//     `auth.mode = "local"` against a tempdir + free port;
//     captures the first-boot admin OTP from stderr (the
//     bootstrap `warn!` line in main.rs); writes
//     `ui/e2e/auth/.engine-state.json` with
//     { baseURL, statedir, pid, bootstrapUsername,
//       bootstrapOtp } for the specs to read.
//   * globalTeardown SIGTERMs the engine and rms the tempdir.
//
// Specs read `process.env.E2E_BASE_URL` (set by globalSetup)
// + `process.env.E2E_BOOTSTRAP_OTP` for the admin password.
// Workers pinned to 1 — every spec mutates the shared user
// table; parallel workers would race.

import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./e2e/auth",
  testIgnore: ["**/fixtures/**", "**/.engine-state.json"],
  workers: 1,
  fullyParallel: false,
  retries: process.env["CI"] ? 1 : 0,
  reporter: process.env["CI"] ? [["github"], ["html", { open: "never" }]] : "list",
  // The auth round-trip is multi-step (login → force reset →
  // admin nav → create user → log out → log back in as them);
  // give each spec generous room without being able to hide
  // a real hang.
  timeout: 60_000,
  expect: {
    timeout: 10_000,
  },
  use: {
    baseURL: process.env["E2E_BASE_URL"] ?? "http://127.0.0.1:18099",
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
  globalSetup: "./e2e/auth/fixtures/global-setup.ts",
  globalTeardown: "./e2e/auth/fixtures/global-teardown.ts",
});
