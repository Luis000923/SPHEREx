// Real-data states — the polling contract exercised end to end by routing
// /api/v1/frames (same preview origin, so the netGuard fixture allows it):
// partial answers that grow on re-request, a transient 502 with backoff,
// a terminal "partial" state, the 404 empty state and the empty diff-mode
// note. The mock mode's simulated partial phase is covered too.
import type { Page } from "@playwright/test";
import {
  expect,
  frameCanvas,
  frameLabelRe,
  mockUrl,
  test,
} from "./fixtures";
import { makeMockFramesResponse } from "../../src/lib/mock";
import type { FramesResponse } from "../../src/lib/types";

function partialResponse(
  count: number,
  opts: { total?: number; mode?: string } = {},
): FramesResponse {
  const total = opts.total ?? 12;
  const full = makeMockFramesResponse({ count: total });
  return {
    ...full,
    query: {
      ...full.query,
      mode: (opts.mode ?? "raw") as FramesResponse["query"]["mode"],
    },
    frames: full.frames.slice(0, count),
    truncated: true,
    total_observations: total,
  };
}

// Registers a /api/v1/frames route that answers each request with the next
// item of `answers` (the last one repeats once exhausted).
async function stubFrames(
  page: Page,
  answers: Array<{ status?: number; body: unknown }>,
): Promise<void> {
  let i = 0;
  await page.route("**/api/v1/frames**", async (route) => {
    const a = answers[Math.min(i, answers.length - 1)];
    i += 1;
    await route.fulfill({
      status: a?.status ?? 200,
      contentType: "application/json",
      body: JSON.stringify(a?.body ?? {}),
    });
  });
}

const COORDS = "ra=133.795&dec=-7.245&fov=0.05";

test("a partial answer shows progress and completes on re-request", async ({
  page,
}) => {
  await stubFrames(page, [
    { body: partialResponse(4) },
    { body: partialResponse(12) },
  ]);
  await page.goto(`/?${COORDS}`);
  // First frames are playable immediately while more arrive.
  await expect(page.getByText("Loading frames 4 / 12")).toBeVisible();
  await expect(frameCanvas(page)).toBeVisible();
  await expect(frameCanvas(page)).toHaveAttribute(
    "aria-label",
    frameLabelRe(1, 12),
    { timeout: 15_000 },
  );
  await expect(page.getByText("Loading frames 4 / 12")).toHaveCount(0);
});

test("a transient 502 retries with backoff instead of erroring", async ({
  page,
}) => {
  await stubFrames(page, [
    {
      status: 502,
      body: { error: { code: "upstream_unavailable", message: "busy" } },
    },
    { body: partialResponse(12) },
  ]);
  await page.goto(`/?${COORDS}`);
  await expect(
    page.getByText(/slow to answer — retrying/),
  ).toBeVisible();
  await expect(frameCanvas(page)).toHaveAttribute(
    "aria-label",
    frameLabelRe(1, 12),
    { timeout: 15_000 },
  );
});

// f11: a series that stops growing below the expected count is the calm
// note — no warning colour, no prominent Retry, a quiet "Check again".
test("a series stuck below the expected count ends in the calm note", async ({
  page,
}) => {
  test.setTimeout(60_000);
  // Partial answers stop growing; the quiet re-check then completes.
  let i = 0;
  await page.route("**/api/v1/frames**", async (route) => {
    i += 1;
    const body = i <= 3 ? partialResponse(4) : partialResponse(12);
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(body),
    });
  });
  await page.goto(`/?${COORDS}`);
  await expect(
    page.getByText(
      "4 of 12 frames available (the archive did not provide the rest).",
    ),
  ).toBeVisible({ timeout: 25_000 });
  // The check-again action picks up what finished downloading meanwhile.
  await page.getByRole("button", { name: "Check again" }).click();
  await expect(frameCanvas(page)).toHaveAttribute(
    "aria-label",
    frameLabelRe(1, 12),
    { timeout: 15_000 },
  );
});

// The "Partial … Retry" warning is reserved for stops a retry can still
// help: here a terminal (never-retried) error after a partial answer.
test("a terminal error after a partial answer keeps the Partial warning", async ({
  page,
}) => {
  test.setTimeout(60_000);
  let i = 0;
  await page.route("**/api/v1/frames**", async (route) => {
    i += 1;
    if (i === 2) {
      await route.fulfill({
        status: 500,
        contentType: "application/json",
        body: JSON.stringify({
          error: { code: "internal", message: "boom" },
        }),
      });
      return;
    }
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(i === 1 ? partialResponse(4) : partialResponse(12)),
    });
  });
  await page.goto(`/?${COORDS}`);
  await expect(page.getByText("Partial: 4 of 12 frames.")).toBeVisible({
    timeout: 25_000,
  });
  // The manual retry asks again and the series completes.
  await page.getByRole("button", { name: "Retry" }).click();
  await expect(frameCanvas(page)).toHaveAttribute(
    "aria-label",
    frameLabelRe(1, 12),
    { timeout: 15_000 },
  );
});

test("404 shows the empty state instead of an error", async ({ page }) => {
  await stubFrames(page, [
    { status: 404, body: { error: { code: "not_found", message: "none" } } },
  ]);
  await page.goto(`/?${COORDS}`);
  await expect(
    page.getByText("No SPHEREx observations cover this position yet"),
  ).toBeVisible();
  await expect(page.getByRole("alert")).toHaveCount(0);
});

test("diff mode with zero comparable frames explains itself", async ({
  page,
}) => {
  test.setTimeout(45_000);
  const empty = partialResponse(0, { total: 24, mode: "diff" });
  await stubFrames(page, [{ body: empty }]);
  await page.goto(`/?${COORDS}&mode=diff`);
  await expect(
    page.getByText("Not enough comparable frames at this position"),
  ).toBeVisible({ timeout: 25_000 });
  await expect(
    page.getByText(/needs several exposures of the same detector and band/),
  ).toBeVisible();
});

test("mock mode simulates the partial-first rhythm", async ({ page }) => {
  await page.goto(mockUrl(`${COORDS}&mock_delay=2500`));
  await expect(page.getByText("Loading frames 4 / 12")).toBeVisible();
  await expect(frameCanvas(page)).toHaveAttribute(
    "aria-label",
    frameLabelRe(1, 12),
  );
});
