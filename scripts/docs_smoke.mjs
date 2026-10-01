// Serve dist at :8080; HANDBOOK_URL can select another handbook URL.
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || "../docs/checks/node_modules/playwright/index.mjs");
const base = process.env.HANDBOOK_URL || "http://127.0.0.1:8080/docs/";
for (let attempt = 0; ; attempt++) {
  try { if ((await fetch(base)).ok) break; } catch { /* server starting */ }
  if (attempt === 49) throw new Error(`Handbook server did not become ready: ${base}`);
  await new Promise(resolve => setTimeout(resolve, 100));
}
const browser = await chromium.launch({ headless: true });
try {
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 }, colorScheme: "light" });
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  await page.goto(base);
  await page.getByRole("heading", { name: "Welcome to Converra", exact: true }).waitFor();
  assert.equal(await page.locator("html").evaluate(el => el.classList.contains("rust")), true);
  const sidebar = page.getByRole("navigation", { name: "Table of contents" });
  await sidebar.getByRole("link", { name: "Your first study" }).click();
  await page.getByRole("heading", { name: "Your first study", exact: true }).waitFor();
  await page.getByRole("button", { name: "Toggle Searchbar" }).click();
  await page.locator("#mdbook-searchbar").pressSequentially("conductor");
  await page.locator("#mdbook-searchresults a").first().waitFor();
  assert.ok((await page.locator("#mdbook-searchresults").innerText()).includes("conductor"));
  assert.ok(!(await sidebar.innerText()).includes("release procedure"));
  await page.getByRole("button", { name: "Change theme" }).click();
  await page.getByRole("menuitem", { name: "Navy", exact: true }).click();
  assert.equal(await page.locator("html").evaluate(el => el.classList.contains("navy")), true);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(new URL("quick-start.html", base).href);
  await page.getByRole("heading", { name: "Your first study", exact: true }).waitFor();
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "No mobile page overflow");
  await page.locator("#mdbook-sidebar-toggle").click();
  await sidebar.getByRole("link", { name: "Read your results" }).click();
  await page.getByRole("heading", { name: "Read your results", exact: true }).waitFor();
  await page.goto(new URL("benchmarks.html", base).href);
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "No mobile benchmark overflow");
  if (process.env.HANDBOOK_SCREENSHOTS) {
    await mkdir(process.env.HANDBOOK_SCREENSHOTS, { recursive: true });
    await page.screenshot({ path: `${process.env.HANDBOOK_SCREENSHOTS}/benchmarks-mobile.png`, fullPage: true });
    await page.setViewportSize({ width: 1280, height: 900 });
    await page.getByRole("button", { name: "Change theme" }).click();
    await page.getByRole("menuitem", { name: "Rust", exact: true }).click();
    await page.screenshot({ path: `${process.env.HANDBOOK_SCREENSHOTS}/benchmarks-desktop.png`, fullPage: true });
    await page.goto(base);
    await page.screenshot({ path: `${process.env.HANDBOOK_SCREENSHOTS}/welcome.png`, fullPage: true });
  }
  assert.deepEqual(errors, [], "Handbook interactions have no JavaScript errors");
  console.log("Handbook navigation, search, themes, and mobile layout pass.");
} finally {
  await browser.close();
}
