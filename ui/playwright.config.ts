// Playwright config. Specs run against a real engine spawned by global
// setup. One-shot config: chromium only, workers=1 (engine state is global),
// retries=0 (failures should be deterministic).
//
// Port: defaults to 18189. Override with E2E_PORT. globalSetup picks up the
// same env var so baseURL and the engine bind always agree.

import { defineConfig, devices } from "@playwright/test";

const PORT = Number(process.env.E2E_PORT ?? 18189);
process.env.E2E_PORT = String(PORT);
const BASE_URL = `http://127.0.0.1:${PORT}`;

export default defineConfig({
  testDir: "./e2e",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: process.env.CI ? "github" : "list",
  timeout: 30_000,
  expect: { timeout: 5_000 },

  globalSetup: "./e2e/global-setup.ts",
  globalTeardown: "./e2e/global-teardown.ts",

  use: {
    baseURL: BASE_URL,
    headless: true,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },

  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
});
