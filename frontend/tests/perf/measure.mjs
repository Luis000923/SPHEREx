// Perf measurement for p2-frontend-perf. Run against a `vite preview`
// server of the production build (default http://127.0.0.1:4173):
//   node tests/perf/measure.mjs
// Measures, with ?mock=1 data:
//  1. navigation -> first painted frame (canvas accessible name "Frame 1 of N")
//  2. history.replaceState calls + long tasks during ~1.5 s of playback
//  3. long tasks recorded between navigation and the full series painting
/* global window, history, document, console, process, Buffer, PerformanceObserver */
import { chromium } from "playwright-core";

const BASE = process.env.PERF_BASE ?? "http://127.0.0.1:4173";

// A contract-valid FramesResponse of w x h float32 frames, served through
// Playwright route interception so the REAL fetch -> zod -> progressive
// decode path is measured (mock mode decodes synchronously instead).
function makePayload(w, h, count) {
  const px = w * h;
  const frames = [];
  for (let i = 0; i < count; i++) {
    const data = new Float32Array(px);
    for (let j = 0; j < px; j++) data[j] = Math.sin(i + j * 0.001) * 50 + 10;
    const b64 = Buffer.from(data.buffer).toString("base64");
    frames.push({
      id: `perf-${i}`,
      obs_mjd_start: 60200 + i,
      obs_mjd_end: 60200 + i + 0.01,
      detector: (i % 6) + 1,
      wavelength_um: 1.1 + i * 0.1,
      collection: "spherex_perf",
      wcs: {
        crpix: [w / 2, h / 2],
        crval: [133.795, -7.245],
        cd: [
          [-0.001, 0],
          [0, -0.001],
        ],
      },
      data_b64: b64,
    });
  }
  return {
    query: { ra_deg: 133.795, dec_deg: -7.245, fov_deg: 0.05, mode: "raw" },
    width: w,
    height: h,
    unit: "MJy/sr",
    frames,
    flag_legend: {},
    truncated: false,
    total_observations: count,
  };
}

async function measure() {
  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });

  const intercept = process.env.PERF_INTERCEPT === "1";
  const count = Number(process.env.PERF_COUNT ?? 12);
  if (intercept) {
    const w = Number(process.env.PERF_W ?? 192);
    const h = Number(process.env.PERF_H ?? 192);
    const body = JSON.stringify(makePayload(w, h, count));
    await page.route("**/api/v1/frames*", (route) =>
      route.fulfill({ contentType: "application/json", body }),
    );
    await page.route("**/api/v1/targets*", (route) =>
      route.fulfill({ contentType: "application/json", body: "[]" }),
    );
  }

  await page.addInitScript(() => {
    // Count history churn + long tasks from before any app code runs.
    window.__perf = { replaceStates: 0, longTasks: [] };
    const orig = history.replaceState.bind(history);
    history.replaceState = (...a) => {
      window.__perf.replaceStates += 1;
      return orig(...a);
    };
    try {
      new PerformanceObserver((list) => {
        for (const e of list.getEntries()) {
          window.__perf.longTasks.push({
            start: Math.round(e.startTime),
            dur: Math.round(e.duration),
          });
        }
      }).observe({ entryTypes: ["longtask"] });
    } catch {
      /* longtask unsupported */
    }
  });

  const t0 = Date.now();
  const query = intercept
    ? `ra=133.795&dec=-7.245&fov=0.05&fps=30`
    : `mock=1&mock_delay=0&fps=30${process.env.PERF_EXTRA ?? ""}`;
  await page.goto(`${BASE}/?${query}`, { waitUntil: "domcontentloaded" });

  // First painted frame: the canvas exposes "Frame 1 of N, ...".
  const canvas = page.getByRole("img", { name: /^Frame \d+ of \d+/ });
  await canvas.waitFor({ state: "attached", timeout: 15000 });
  const firstFrameMs = Date.now() - t0;
  const aria0 = await canvas.getAttribute("aria-label");
  const m = /of (\d+)/.exec(aria0 ?? "");
  const total = m ? Number(m[1]) : 0;

  // Wait until the full series decoded (aria label "of N").
  await page.waitForFunction(
    (want) =>
      new RegExp(`of ${want}\\b`).test(
        document.querySelector(".tl-canvas")?.getAttribute("aria-label") ?? "",
      ),
    count,
    { timeout: 30000 },
  );
  const allFramesMs = Date.now() - t0;

  // Playback: 1.5 s at 30 fps.
  await page.getByRole("button", { name: "Play" }).click();
  await page.waitForTimeout(1500);
  await page.getByRole("button", { name: "Pause" }).click();
  await page.waitForTimeout(300);

  const stats = await page.evaluate(() => window.__perf);
  await browser.close();

  const totalLongMs = stats.longTasks.reduce((s, t) => s + t.dur, 0);
  return {
    firstFrameMs,
    allFramesMs,
    replaceStates: stats.replaceStates,
    longTasks: stats.longTasks,
    totalLongTaskMs: totalLongMs,
  };
}

const out = await measure();
console.log(JSON.stringify(out, null, 2));
