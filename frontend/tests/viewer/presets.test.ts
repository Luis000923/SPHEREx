// Preset targets hold epoch-2026.0 positions (propagated from SIMBAD J2000
// for the high proper-motion targets) — the same table the backend serves.
import { describe, expect, it } from "vitest";
import { PRESET_TARGETS, menuTargetsFromServer } from "../../src/lib/presets";
import { isValidFov, parseDecToDeg, parseRaToDeg } from "../../src/lib/coords";

const EXPECTED_DEG: Record<string, { ra: number; dec: number; fov: number }> = {
  "barnards-star": { ra: 269.4462583, dec: 4.768325, fov: 0.1 },
  "epsilon-indi-b": { ra: 331.0966708, dec: -56.80065, fov: 0.1 },
  "wise-0855": { ra: 133.7597667, dec: -7.2421833, fov: 0.1 },
  "rw-aurigae": { ra: 76.9565875, dec: 30.4012917, fov: 0.05 },
  "r-draconis": { ra: 248.1672333, dec: 66.755025, fov: 0.05 },
};

describe("PRESET_TARGETS", () => {
  it("has the five official targets with unique ids", () => {
    expect(PRESET_TARGETS).toHaveLength(5);
    expect(new Set(PRESET_TARGETS.map((t) => t.id)).size).toBe(5);
  });

  it("stores parseable, in-range coordinates", () => {
    for (const t of PRESET_TARGETS) {
      const ra = parseRaToDeg(t.ra);
      const dec = parseDecToDeg(t.dec);
      expect(ra, `${t.id} ra`).not.toBeNull();
      expect(dec, `${t.id} dec`).not.toBeNull();
      expect(isValidFov(t.fov), `${t.id} fov`).toBe(true);
    }
  });

  it("matches the epoch-2026.0 backend table", () => {
    for (const t of PRESET_TARGETS) {
      const exp = EXPECTED_DEG[t.id];
      expect(exp, `missing expectation for ${t.id}`).toBeDefined();
      if (exp === undefined) continue;
      expect(parseRaToDeg(t.ra)).toBeCloseTo(exp.ra, 4);
      expect(parseDecToDeg(t.dec)).toBeCloseTo(exp.dec, 4);
      expect(t.fov).toBe(exp.fov);
    }
  });

  it("keeps the official challenge coordinates visible for WISE 0855", () => {
    const wise = PRESET_TARGETS.find((t) => t.id === "wise-0855");
    expect(wise?.note).toContain("08h55m10.831s");
    expect(wise?.note).toContain("-07d14m42.53s");
    expect(wise?.note).toContain("AllWISE");
  });

  // f11: real-data QA found WISE 0855 invisible in raw frames — the note
  // must set expectations instead of letting users think the tool failed.
  it("warns that WISE 0855 is faint and near the pixel-scale limit", () => {
    const wise = PRESET_TARGETS.find((t) => t.id === "wise-0855");
    expect(wise?.note).toMatch(/faint/);
    expect(wise?.note).toContain("D5");
    expect(wise?.note).toMatch(/pixel/);
  });
});

// f10 regression: the header menu used to re-format /api/v1/targets degrees
// with 2/1 decimals ("17h57m47.10s"), a different string than the baked cache
// key ("17h57m47.102s"), so every preset missed the cache (502 offline).
describe("menuTargetsFromServer", () => {
  const server = [
    { id: "barnards-star", name: "Barnard's Star", ra_deg: 269.44625833333333, dec_deg: 4.768325, fov_deg: 0.1, note: "server note" },
    { id: "new-target", name: "New", ra_deg: 10.5, dec_deg: -20.25, fov_deg: 0.05 },
  ];

  it("keeps the built-in sexagesimal strings byte-identical for known ids", () => {
    const out = menuTargetsFromServer(server);
    const barnard = PRESET_TARGETS.find((t) => t.id === "barnards-star");
    expect(out[0]?.ra).toBe(barnard?.ra);
    expect(out[0]?.dec).toBe(barnard?.dec);
    expect(out[0]?.ra).toBe("17h57m47.102s");
    expect(out[0]?.dec).toBe("+04d46m05.97s");
    expect(out[0]?.label).toBe("Barnard's Star");
    expect(out[0]?.note).toBe("server note");
  });

  it("formats unknown server targets with full precision", () => {
    const out = menuTargetsFromServer(server);
    expect(out[1]?.ra).toBe("00h42m00.000s");
    expect(out[1]?.dec).toBe("-20d15m00.00s");
    expect(out[1]?.fov).toBe(0.05);
  });

  it("every built-in preset round-trips unchanged through the server table", () => {
    const fromServer = PRESET_TARGETS.map((t) => ({
      id: t.id,
      name: t.label,
      ra_deg: parseRaToDeg(t.ra) ?? 0,
      dec_deg: parseDecToDeg(t.dec) ?? 0,
      fov_deg: t.fov,
    }));
    const out = menuTargetsFromServer(fromServer);
    out.forEach((t, i) => {
      expect(t.ra).toBe(PRESET_TARGETS[i]?.ra);
      expect(t.dec).toBe(PRESET_TARGETS[i]?.dec);
    });
  });
});
