// Test 6 — click-to-spectrum: the panel opens with the full mock point
// list, Escape closes it, and while open it must not cover the primary
// controls (Go, search inputs, Controls toggle). The flagged-outlier
// chart state (robust y-range, hollow marker, off-scale note, flag names
// in the hover tooltip, 5 px default aperture) is exercised against a
// routed /api/v1/spectrum answer.
import { makeMockFramesResponse } from "../../src/lib/mock";
import { makeMockSpectrum } from "../../src/components/spectrum/mockSpectrum";
import { prepareSpectrum } from "../../src/components/spectrum/prepare";
import {
  expect,
  expectNotCovered,
  frameCanvas,
  mockUrl,
  test,
} from "./fixtures";

// Number of usable points in the synthetic spectrum, computed with the same
// preparation the panel uses (one mock point is NaN on purpose).
const EXPECTED_POINTS = prepareSpectrum(makeMockSpectrum(0, 0).points).validCount;

test("canvas click opens the spectrum panel with every mock point", async ({
  page,
}) => {
  await page.goto(mockUrl("ra=133.795&dec=-7.245&fov=0.05"));
  await frameCanvas(page).click();

  const panel = page.getByRole("complementary", { name: "Spectrum panel" });
  await expect(panel).toBeVisible();
  await expect(
    panel.getByText(new RegExp(`^${EXPECTED_POINTS} points`)),
  ).toBeVisible();

  await page.keyboard.press("Escape");
  await expect(panel).toHaveCount(0);
});

test("controls stay clickable while the spectrum panel is open", async ({
  page,
}) => {
  await page.goto(mockUrl("ra=133.795&dec=-7.245&fov=0.05"));
  await frameCanvas(page).click();
  await expect(
    page.getByRole("complementary", { name: "Spectrum panel" }),
  ).toBeVisible();

  // The docked spectrum panel must not intercept the primary controls.
  await expectNotCovered(
    page.getByRole("button", { name: "Load time-lapse" }),
    "Go button",
  );
  await expectNotCovered(
    page.getByLabel("Right ascension"),
    "RA input",
  );
  await expectNotCovered(
    page.getByRole("button", { name: "Controls" }),
    "Controls panel toggle",
  );
});

// f11: one flagged outlier (TRANSIENT, SUR_ERROR, PERSIST at 103.8 vs
// ~0.3 MJy/sr, as seen on real data) used to set the axis. Now it is a
// hollow off-scale marker and its flag names show in the hover tooltip.
test("a flagged outlier stays off-scale and shows flag names on hover", async ({
  page,
}) => {
  const spectrum = {
    ra_deg: 133.795,
    dec_deg: -7.245,
    unit: "MJy/sr",
    points: [1, 2, 3, 4, 5].map((um) => {
      const flagged = um === 3;
      return {
        wavelength_um: um,
        value: flagged ? 103.8 : 0.3 + um * 0.01,
        sigma: 0.02,
        obs_mjd: 60200 + um,
        detector: 1,
        flags: flagged ? ["TRANSIENT", "SUR_ERROR", "PERSIST"] : [],
      };
    }),
  };
  let spectrumUrl = "";
  await page.route("**/api/v1/frames**", async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(makeMockFramesResponse({ count: 4 })),
    });
  });
  await page.route("**/api/v1/spectrum**", async (route) => {
    spectrumUrl = route.request().url();
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(spectrum),
    });
  });

  await page.goto("/?ra=133.795&dec=-7.245&fov=0.05");
  await frameCanvas(page).click();
  const panel = page.getByRole("complementary", { name: "Spectrum panel" });
  await expect(panel).toBeVisible();
  await expect(panel.getByText(/^5 points/)).toBeVisible();
  // f11: the spectrum request uses the 5 px aperture by default.
  expect(spectrumUrl).toContain("aperture_px=5");

  // Hover the chart centre: the flagged outlier sits at 3.0 µm, the
  // middle of the five points.
  const plot = panel.locator(".sp-plot");
  const box = await plot.boundingBox();
  expect(box).not.toBeNull();
  await page.mouse.move(
    box!.x + box!.width * 0.5,
    box!.y + box!.height * 0.4,
  );
  await expect(page.getByText(/flags: TRANSIENT/)).toBeVisible();
  await expect(page.getByText(/off scale/)).toBeVisible();
});
