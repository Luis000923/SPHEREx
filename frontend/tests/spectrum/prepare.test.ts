import { describe, expect, it } from "vitest";
import {
  MOLECULAR_BANDS,
  bandRects,
  detectorsPresent,
  flagCounts,
  layoutBandLabels,
  prepareSpectrum,
  robustYRange,
  type BandRect,
} from "../../src/components/spectrum/prepare";
import { makeMockSpectrum } from "../../src/components/spectrum/mockSpectrum";
import type { SpectrumPoint } from "../../src/components/spectrum/types";

function pt(partial: Partial<SpectrumPoint> = {}): SpectrumPoint {
  return {
    wavelength_um: 1.0,
    value: 10,
    sigma: 0.5,
    obs_mjd: 60000,
    detector: 1,
    ...partial,
  };
}

describe("prepareSpectrum", () => {
  it("returns empty arrays for an empty payload", () => {
    const p = prepareSpectrum([]);
    expect(p.wavelengthUm).toEqual([]);
    expect(p.flux).toEqual([]);
    expect(p.sigma).toEqual([]);
    expect(p.validCount).toBe(0);
  });

  it("sorts points by wavelength", () => {
    const p = prepareSpectrum([
      pt({ wavelength_um: 3.0 }),
      pt({ wavelength_um: 1.0 }),
      pt({ wavelength_um: 2.0 }),
    ]);
    expect(p.wavelengthUm).toEqual([1, 2, 3]);
    expect(p.validCount).toBe(3);
  });

  it("maps NaN/Inf flux to null (chart gap) and does not count it", () => {
    const p = prepareSpectrum([
      pt({ value: NaN }),
      pt({ value: Infinity }),
      pt({ value: 5 }),
      pt({ value: -Infinity }),
    ]);
    expect(p.flux).toEqual([null, null, 5, null]);
    expect(p.validCount).toBe(1);
  });

  it("nulls non-positive or non-finite sigma", () => {
    const p = prepareSpectrum([
      pt({ sigma: NaN }),
      pt({ sigma: 0 }),
      pt({ sigma: -1 }),
      pt({ sigma: 0.25 }),
      pt({ sigma: Infinity }),
    ]);
    expect(p.sigma).toEqual([null, null, null, 0.25, null]);
  });

  it("handles the synthetic mock payload including its planted NaN", () => {
    const res = makeMockSpectrum(133.8, -7.24, { count: 30 });
    const p = prepareSpectrum(res.points);
    expect(p.wavelengthUm).toHaveLength(30);
    expect(p.validCount).toBe(29); // last point is NaN by construction
    expect(p.sigma[0]).toBeNull(); // first point has NaN sigma
    for (let i = 1; i < p.wavelengthUm.length; i++) {
      expect(p.wavelengthUm[i]!).toBeGreaterThanOrEqual(p.wavelengthUm[i - 1]!);
    }
  });
});

describe("robustYRange", () => {
  // f11: a single flagged outlier (TRANSIENT/SUR_ERROR/PERSIST, 103.8 vs
  // ~0.3 MJy/sr on real data) squashed the whole chart. The axis range is
  // now the 2nd-98th percentile of the unflagged points plus 10% margin.

  it("is null for an empty or all-NaN payload", () => {
    expect(robustYRange([])).toBeNull();
    expect(robustYRange([pt({ value: NaN }), pt({ value: NaN })])).toBeNull();
  });

  it("excludes a flagged outlier so the bulk of the data fills the axis", () => {
    const points = [
      ...Array.from({ length: 22 }, (_, i) =>
        pt({ wavelength_um: 1 + i * 0.1, value: 0.3 + i * 0.01 }),
      ),
      pt({ wavelength_um: 4.93, value: 103.8, flags: ["TRANSIENT", "SUR_ERROR", "PERSIST"] }),
    ];
    const r = robustYRange(points);
    expect(r).not.toBeNull();
    expect(r!.max).toBeLessThan(1);
    expect(r!.min).toBeLessThan(0.4);
  });

  it("clips even an unflagged extreme value through the percentile", () => {
    const points = [
      ...Array.from({ length: 22 }, (_, i) =>
        pt({ wavelength_um: 1 + i * 0.1, value: 0.3 + i * 0.01 }),
      ),
      pt({ wavelength_um: 4.9, value: 100 }),
    ];
    const r = robustYRange(points);
    expect(r).not.toBeNull();
    // p98 interpolates between the bulk and the outlier but the 10%
    // margin cannot reach 100, so the top point is off-scale.
    expect(r!.max).toBeLessThan(100);
    expect(r!.max).toBeGreaterThan(0.5);
    expect(r!.min).toBeLessThan(0.4);
  });

  it("ignores NaN values while choosing the percentile pool", () => {
    const points = [
      pt({ value: NaN }),
      pt({ value: 1 }),
      pt({ value: 2 }),
      pt({ value: NaN }),
      pt({ value: 3 }),
    ];
    const r = robustYRange(points);
    expect(r).not.toBeNull();
    expect(r!.min).toBeLessThanOrEqual(1);
    expect(r!.max).toBeGreaterThanOrEqual(3);
  });

  it("centres the range on a single point instead of collapsing", () => {
    const r = robustYRange([pt({ value: 5 })]);
    expect(r).not.toBeNull();
    expect(r!.min).toBeLessThan(5);
    expect(r!.max).toBeGreaterThan(5);
  });

  it("widens the range when every value is equal", () => {
    const r = robustYRange([pt({ value: 2 }), pt({ value: 2 }), pt({ value: 2 })]);
    expect(r).not.toBeNull();
    expect(r!.min).toBeLessThan(2);
    expect(r!.max).toBeGreaterThan(2);
  });

  it("keeps a nonzero span around a zero constant series", () => {
    const r = robustYRange([pt({ value: 0 }), pt({ value: 0 })]);
    expect(r).not.toBeNull();
    expect(r!.max).toBeGreaterThan(r!.min);
  });

  it("falls back to all points when every finite point is flagged", () => {
    const points = [
      pt({ value: 1, flags: ["TRANSIENT"] }),
      pt({ value: 50, flags: ["SUR_ERROR"] }),
      pt({ value: 100, flags: ["PERSIST"] }),
    ];
    const r = robustYRange(points);
    expect(r).not.toBeNull();
    expect(r!.min).toBeLessThanOrEqual(1);
    expect(r!.max).toBeGreaterThanOrEqual(100);
  });

  it("leaves the headroom margin proportional to the data span", () => {
    const points = Array.from({ length: 10 }, (_, i) => pt({ value: i }));
    const r = robustYRange(points)!;
    const mid = (r.min + r.max) / 2;
    expect(mid).toBeCloseTo(4.5, 5);
    expect(r.max - r.min).toBeGreaterThan(9);
  });
});

describe("detectorsPresent", () => {
  it("returns a sorted unique list", () => {
    expect(
      detectorsPresent([
        pt({ detector: 5 }),
        pt({ detector: 1 }),
        pt({ detector: 5 }),
        pt({ detector: 3 }),
      ]),
    ).toEqual([1, 3, 5]);
  });
  it("is empty for no points", () => {
    expect(detectorsPresent([])).toEqual([]);
  });
});

describe("flagCounts", () => {
  it("counts names verbatim and sorts by frequency", () => {
    const c = flagCounts([
      pt({ flags: ["A", "B"] }),
      pt({ flags: ["A"] }),
      pt({}),
      pt({ flags: [] }),
    ]);
    expect(c).toEqual([
      { name: "A", count: 2 },
      { name: "B", count: 1 },
    ]);
  });
});

describe("MOLECULAR_BANDS", () => {
  it("has the four required species", () => {
    const keys = MOLECULAR_BANDS.map((b) => b.key);
    for (const k of ["h2o", "co2", "co", "ch4"]) expect(keys).toContain(k);
  });
  it("ranges are valid and inside the SPHEREx 0.75-5 um window", () => {
    for (const b of MOLECULAR_BANDS) {
      const [lo, hi] = b.rangeUm;
      expect(lo).toBeGreaterThanOrEqual(0.75);
      expect(hi).toBeLessThanOrEqual(5.0);
      expect(hi).toBeGreaterThan(lo);
    }
  });
});

describe("bandRects", () => {
  const linear = (lo: number, hi: number, w: number) => (um: number) =>
    ((um - lo) / (hi - lo)) * w;

  it("maps visible bands to pixel rects", () => {
    const rects = bandRects(MOLECULAR_BANDS, linear(2, 5, 600), 2, 5);
    expect(rects.map((r) => r.key)).toEqual(["h2o", "ch4", "co2", "co"]);
    const h2o = rects[0]!;
    expect(h2o.x0).toBeCloseTo(((2.95 - 2) / 3) * 600, 6);
    expect(h2o.x1).toBeCloseTo(((3.2 - 2) / 3) * 600, 6);
  });

  it("skips bands fully outside the visible range", () => {
    const rects = bandRects(MOLECULAR_BANDS, linear(0.75, 2, 400), 0.75, 2);
    expect(rects).toEqual([]);
  });

  it("handles a reversed axis (x1 > x0 swapped input)", () => {
    const rects = bandRects(MOLECULAR_BANDS, linear(5, 2, 600), 5, 2);
    for (const r of rects) expect(r.x1).toBeGreaterThanOrEqual(r.x0);
    expect(rects).toHaveLength(4);
  });

  it("skips non-finite projections", () => {
    const rects = bandRects(
      MOLECULAR_BANDS,
      () => NaN,
      0.75,
      5,
    );
    expect(rects).toEqual([]);
  });
});

describe("layoutBandLabels", () => {
  const rect = (key: string, x0: number, x1: number): BandRect => ({
    key,
    label: key.toUpperCase(),
    x0,
    x1,
  });
  const w30 = () => 30; // every label measures 30 px

  it("keeps well-separated labels on the top row", () => {
    const ls = layoutBandLabels([rect("a", 10, 40), rect("b", 200, 240)], w30);
    expect(ls.map((l) => [l.key, l.row])).toEqual([
      ["a", 0],
      ["b", 0],
    ]);
  });

  it("staggers an adjacent label (H2O/CH4 case) onto the next row", () => {
    // h2o's label spans 104-134; ch4 anchored at 109 collides -> row 1.
    const ls = layoutBandLabels(
      [rect("h2o", 100, 140), rect("ch4", 105, 150)],
      w30,
    );
    expect(ls.find((l) => l.key === "h2o")?.row).toBe(0);
    expect(ls.find((l) => l.key === "ch4")?.row).toBe(1);
  });

  it("drops a label that collides on every row", () => {
    const ls = layoutBandLabels(
      [rect("a", 0, 20), rect("b", 10, 30), rect("c", 20, 40)],
      w30,
      2,
    );
    expect(ls.map((l) => l.key)).toEqual(["a", "b"]);
    expect(ls.map((l) => l.row)).toEqual([0, 1]);
  });

  it("never emits two labels closer than the gap on the same row", () => {
    // Narrow-chart stress: five bands squeezed together, wide labels.
    const rects = [
      rect("a", 0, 30),
      rect("b", 12, 42),
      rect("c", 24, 54),
      rect("d", 36, 66),
      rect("e", 48, 78),
    ];
    for (const maxRows of [1, 2, 3]) {
      const gap = 6;
      const width = 60;
      const ls = layoutBandLabels(rects, () => width, maxRows, gap);
      const byRow = new Map<number, typeof ls>();
      for (const l of ls) {
        const row = byRow.get(l.row) ?? [];
        row.push(l);
        byRow.set(l.row, row);
      }
      for (const row of byRow.values()) {
        const sorted = [...row].sort((p, q) => p.x - q.x);
        for (let i = 1; i < sorted.length; i++) {
          expect(sorted[i]!.x - sorted[i - 1]!.x).toBeGreaterThanOrEqual(
            width + gap,
          );
        }
      }
    }
  });
});
