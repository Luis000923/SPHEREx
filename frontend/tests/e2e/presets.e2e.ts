// Test 3 — the five official targets are reachable from the header target
// menu and load their RA/Dec into the search inputs. Coordinates are the
// epoch-2026.0 presets (propagated for proper motion), same table the
// backend serves from /api/v1/targets.
import { expect, mockUrl, test } from "./fixtures";

const TARGETS = [
  { label: "Barnard's Star", ra: "17h57m47.102s", dec: "+04d46m05.97s" },
  { label: "Epsilon Indi B", ra: "22h04m23.201s", dec: "-56d48m02.34s" },
  { label: "WISE 0855", ra: "08h55m02.344s", dec: "-07d14m31.86s" },
  { label: "RW Aurigae", ra: "05h07m49.581s", dec: "+30d24m04.65s" },
  { label: "R Draconis", ra: "16h32m40.136s", dec: "+66d45m18.09s" },
] as const;

test("all five official targets load their coordinates from the menu", async ({
  page,
}) => {
  await page.goto(mockUrl());
  const raInput = page.getByLabel("Right ascension");
  const decInput = page.getByLabel("Declination");

  // The About dialog also contains a "Targets" heading; the menu trigger is
  // a <summary> disclosure, so match tag + text.
  const menu = page.locator("summary", { hasText: "Targets" });
  for (const t of TARGETS) {
    await menu.click();
    // Menu items read "label ra · dec"; the same-named preset pills in the
    // search bar are only "label", so matching on the coordinates picks the
    // menu entry unambiguously.
    await page
      .getByRole("button", {
        name: new RegExp(`${t.label} ${t.ra}`),
      })
      .click();
    await expect(raInput).toHaveValue(t.ra);
    await expect(decInput).toHaveValue(t.dec);
  }
});
