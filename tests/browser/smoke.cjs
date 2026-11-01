// Headless-browser smoke test of the built web client, against a running server.
//
// Loads the app in Chromium (WebGL2 through SwiftShader, so no GPU is needed) and checks that
// the WebAssembly client starts (it removes the #loader card), loads the demo street from
// /api/demo, starts the coverage worker, and logs no errors or panics while doing so. Map
// tiles are answered here with a blank PNG, so the test makes no requests to OpenStreetMap.
//
//   OBS_URL=http://127.0.0.1:8080 node smoke.cjs
//
// Environment:
//   OBS_URL      the server (default http://127.0.0.1:8080)
//   CHROME_PATH  a Chrome or Chromium executable (default: the installed Google Chrome)
//   OUT_DIR      where the screenshot and console log go (default ./out)
//   TIMEOUT_MS   how long the client may take to start (default 180000; SwiftShader is slow)
"use strict";

const fs = require("node:fs");
const path = require("node:path");
const { chromium } = require("playwright-core");

const url = process.env.OBS_URL || "http://127.0.0.1:8080";
const outDir = process.env.OUT_DIR || path.join(__dirname, "out");
const timeout = Number(process.env.TIMEOUT_MS || 180000);
// After the client is up, keep watching this long for a late panic.
const SETTLE_MS = 5000;
// A 1x1 transparent PNG.
const BLANK_TILE = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=",
  "base64",
);

async function main() {
  fs.mkdirSync(outDir, { recursive: true });
  const browser = await chromium.launch({
    ...(process.env.CHROME_PATH ? { executablePath: process.env.CHROME_PATH } : { channel: "chrome" }),
    headless: true,
    // WebGL2 on the CPU (no GPU on CI runners), and none of Chrome's own background traffic.
    args: [
      "--use-angle=swiftshader",
      "--enable-unsafe-swiftshader",
      "--ignore-gpu-blocklist",
      "--disable-background-networking",
      "--disable-component-update",
      "--no-first-run",
    ],
  });
  const log = [];
  const errors = [];
  try {
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 } });
    const page = await context.newPage();
    const origin = new URL(url).origin;

    page.on("console", (msg) => {
      const line = `[${msg.type()}] ${msg.text()}`;
      log.push(line);
      if (msg.type() === "error") errors.push(`console: ${msg.text()}`);
    });
    page.on("pageerror", (err) => {
      log.push(`[pageerror] ${err.message}`);
      errors.push(`uncaught: ${err.message}`);
    });
    page.on("requestfailed", (req) => {
      if (req.url().startsWith(origin)) errors.push(`request failed: ${req.url()} (${req.failure()?.errorText})`);
    });
    page.on("response", (res) => {
      if (res.url().startsWith(origin) && res.status() >= 400) errors.push(`HTTP ${res.status()}: ${res.url()}`);
    });
    // The client's own tile requests; nothing reaches the tile server.
    await page.route(`${origin}/api/tiles/**`, (route) =>
      route.fulfill({ status: 200, contentType: "image/png", body: BLANK_TILE }),
    );

    const demo = page.waitForResponse((r) => new URL(r.url()).pathname === "/api/demo", { timeout });
    const worker = page.waitForResponse((r) => new URL(r.url()).pathname === "/coverage-worker_bg.wasm", { timeout });
    // Awaited below; if an earlier step fails first, their rejections are not the error to report.
    demo.catch(() => {});
    worker.catch(() => {});

    const t0 = Date.now();
    await page.goto(url, { waitUntil: "load" });
    const webgl2 = await page.evaluate(() => !!document.createElement("canvas").getContext("webgl2"));
    if (!webgl2) throw new Error("this browser has no WebGL2; the test needs SwiftShader (see the launch args)");

    await page.locator("#loader").waitFor({ state: "detached", timeout });
    console.log(`client started in ${((Date.now() - t0) / 1000).toFixed(1)} s`);
    const demoRes = await demo;
    if (demoRes.status() !== 200) throw new Error(`/api/demo answered ${demoRes.status()}`);
    console.log(`demo street loaded (${(await demoRes.body()).length} bytes)`);
    const workerRes = await worker;
    if (workerRes.status() !== 200) throw new Error(`coverage-worker_bg.wasm answered ${workerRes.status()}`);
    console.log("coverage worker started");

    await page.waitForTimeout(SETTLE_MS);
    if (await page.locator("#gate").isVisible()) throw new Error("the small-screen gate is showing at 1280x800");
    await page.screenshot({ path: path.join(outDir, "smoke.png") });
  } catch (err) {
    errors.push(err.message);
  } finally {
    fs.writeFileSync(path.join(outDir, "console.log"), log.join("\n") + "\n");
    await browser.close();
  }

  if (errors.length) {
    console.error(`FAILED (${errors.length}):`);
    for (const e of errors) console.error(`  - ${e}`);
    console.error(`console log: ${path.join(outDir, "console.log")}`);
    process.exit(1);
  }
  console.log("browser smoke test passed");
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
