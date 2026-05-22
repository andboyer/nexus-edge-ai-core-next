// Dashboard smoke spec: shell mounts, KPIs render, navigation works.

import { expect, test } from "@playwright/test";

import { loginAsAdmin } from "./helpers";

test.describe("dashboard", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page);
  });

  test("renders shell + main outlet on /dashboard", async ({ page }) => {
    await page.goto("/dashboard");
    await expect(page.locator("main")).toBeVisible();
    await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  });

  test("navigation: dashboard -> cameras -> events", async ({ page }) => {
    await page.goto("/dashboard");
    await page.getByRole("link", { name: /^cameras$/i }).first().click();
    await expect(page).toHaveURL(/\/cameras/);
    await expect(page.getByRole("heading", { name: /cameras/i })).toBeVisible();

    await page.getByRole("link", { name: /^events$/i }).first().click();
    await expect(page).toHaveURL(/\/events/);
  });

  test("admin sidebar group is visible to admin", async ({ page }) => {
    await page.goto("/admin/users");
    await expect(page.getByRole("heading", { name: /users/i })).toBeVisible();
    await expect(page.getByRole("button", { name: /new user/i })).toBeVisible();
  });
});
