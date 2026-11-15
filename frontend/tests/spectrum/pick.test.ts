import { describe, expect, it } from "vitest";
import {
  canvasPickToSky,
  clientPointToPixel,
  pixelToSky,
} from "../../src/components/spectrum/pick";
import type { Wcs } from "../../src/lib/types";

// North-up cutout: 100x100 px over a 0.1 deg field centered on the crval.
const wcs: Wcs = {
  crpix: [50, 50],
  crval: [133.795129, -7.245147],
  cd: [[-0.001, 0], [0, 0.001]],
};

describe("pixelToSky", () => {
  it("maps the reference pixel to crval (1-based crpix)", () => {
    // FITS pixel 50 -> 0-based index 49
    const sky = pixelToSky(wcs, 49, 49);
    expect(sky).not.toBeNull();
    expect(sky!.ra_deg).toBeCloseTo(133.795129, 6);
    expect(sky!.dec_deg).toBeCloseTo(-7.245147, 6);
  });

  it("applies the CD matrix (negative RA slope)", () => {
    const sky = pixelToSky(wcs, 59, 49); // +10 px in x
    // TAN: an RA offset on the sky is xi / cos(dec).
    const cosDec = Math.cos((-7.245147 * Math.PI) / 180);
    expect(sky!.ra_deg).toBeCloseTo(133.795129 - 0.01 / cosDec, 6);
    expect(sky!.dec_deg).toBeCloseTo(-7.245147, 6);
  });

  it("supports rotated CD terms", () => {
    const rot: Wcs = {
      crpix: [10, 10],
      crval: [10, 20],
      cd: [[0, -0.001], [0.001, 0]],
    };
    const sky = pixelToSky(rot, 19, 9); // xi=10, eta=0
    expect(sky!.ra_deg).toBeCloseTo(10, 6);
    expect(sky!.dec_deg).toBeCloseTo(20.01, 6);
  });

  it("wraps RA into [0, 360)", () => {
    const s: Wcs = {
      crpix: [1, 1],
      crval: [359.9995, 0],
      cd: [[0.001, 0], [0, 0.001]],
    };
    const sky = pixelToSky(s, 2, 0); // xi=2 -> 359.9995+0.002 = 360.0015
    expect(sky!.ra_deg).toBeCloseTo(0.0015, 6);
  });

  it("returns null for non-finite input", () => {
    expect(pixelToSky(wcs, NaN, 0)).toBeNull();
    expect(pixelToSky(wcs, 0, Infinity)).toBeNull();
    const bad: Wcs = {
      crpix: [NaN, 0],
      crval: [0, 0],
      cd: [[1, 0], [0, 1]],
    };
    expect(pixelToSky(bad, 0, 0)).toBeNull();
  });
});

describe("clientPointToPixel", () => {
  const rect = { left: 10, top: 20, width: 200, height: 100 };

  it("scales client coords into pixel coords", () => {
    const p = clientPointToPixel(rect, 110, 70, 400, 200);
    expect(p).toEqual({ x: 200, y: 100 }); // rect center
  });

  it("maps rect corners to pixel corners", () => {
    expect(clientPointToPixel(rect, 10, 20, 400, 200)).toEqual({ x: 0, y: 0 });
    expect(clientPointToPixel(rect, 210, 120, 400, 200)).toEqual({
      x: 400,
      y: 200,
    });
  });

  // f10 regression: the canvas is drawn with `object-fit: contain`, so a
  // square cutout in a wide stage is letterboxed. Mapping over the whole
  // element rect put an off-centre click ~14 px (26") off in x on real data.
  it("accounts for object-fit: contain letterboxing (wide rect)", () => {
    const wide = { left: 0, top: 0, width: 1118, height: 823 };
    const side = 823;
    const x0 = (1118 - side) / 2;
    const p = clientPointToPixel(wide, x0 + side * (150.5 / 192), side * (96.5 / 192), 192, 192);
    expect(p?.x).toBeCloseTo(150.5, 6);
    expect(p?.y).toBeCloseTo(96.5, 6);
    expect(clientPointToPixel(wide, 1118 / 2, 823 / 2, 192, 192)).toEqual({ x: 96, y: 96 });
  });

  it("accounts for letterboxing in a tall rect", () => {
    const tall = { left: 5, top: 10, width: 300, height: 500 };
    const p = clientPointToPixel(tall, 5 + 300, 10 + 100 + 300, 192, 192);
    expect(p?.x).toBeCloseTo(192, 6);
    expect(p?.y).toBeCloseTo(192, 6);
  });

  it("returns null for clicks in the letterbox bars", () => {
    const wide = { left: 0, top: 0, width: 1118, height: 823 };
    expect(clientPointToPixel(wide, 10, 400, 192, 192)).toBeNull();
    expect(clientPointToPixel(wide, 1110, 400, 192, 192)).toBeNull();
  });

  it("returns null on degenerate rects and sizes", () => {
    expect(
      clientPointToPixel({ ...rect, width: 0 }, 0, 0, 400, 200),
    ).toBeNull();
    expect(clientPointToPixel(rect, 0, 0, 0, 200)).toBeNull();
    expect(clientPointToPixel(rect, NaN, 0, 400, 200)).toBeNull();
  });
});

describe("canvasPickToSky", () => {
  it("combines rect scaling and WCS projection", () => {
    const canvas = {
      getBoundingClientRect: () => ({
        left: 0,
        top: 0,
        width: 200,
        height: 200,
        right: 200,
        bottom: 200,
        x: 0,
        y: 0,
        toJSON: () => ({}),
      }),
    } as unknown as HTMLCanvasElement;
    // Client (100,100) -> pixel (50,50) in a 100x100 image = FITS pixel 51,
    // one pixel past crpix=50 -> 1 * 0.001 deg offset.
    const sky = canvasPickToSky(canvas, 100, 100, wcs, 100, 100);
    expect(sky).not.toBeNull();
    const cosDec = Math.cos((-7.245147 * Math.PI) / 180);
    expect(sky!.ra_deg).toBeCloseTo(133.795129 - 0.001 / cosDec, 5);
    expect(sky!.dec_deg).toBeCloseTo(-7.245147 + 0.001, 5);
  });
});

// Regression: pixelToSky used to add CD offsets to RA directly. On the sky
// an RA offset is xi / cos(dec), so at R Draconis (dec +66.75) a click 10 px
// east was placed 2.5x too close to the centre in RA and the spectrum came
// from the wrong star. WCS below matches what the backend sends: cd22 < 0,
// row 0 is north.
describe("pixelToSky TAN inverse at high declination", () => {
  const scale = 0.05 / 120;
  const wcs = {
    crpix: [60.5, 60.5] as [number, number],
    crval: [248.1675, 66.75472] as [number, number],
    cd: [
      [-scale, 0],
      [0, -scale],
    ] as [[number, number], [number, number]],
  };
  it("scales RA offsets by 1/cos(dec)", () => {
    // 0-based x = 49.5 is 10 px left of crpix (east, cd11 < 0).
    const p = pixelToSky(wcs, 49.5, 59.5);
    expect(p).not.toBeNull();
    if (p === null) return;
    const dRaCosDec =
      (p.ra_deg - wcs.crval[0]) * Math.cos((p.dec_deg * Math.PI) / 180);
    expect(dRaCosDec).toBeCloseTo(10 * scale, 7);
    expect(p.dec_deg).toBeCloseTo(wcs.crval[1], 5);
  });
  it("row 0 is north when cd22 < 0", () => {
    const top = pixelToSky(wcs, 59.5, 0);
    const bottom = pixelToSky(wcs, 59.5, 119);
    expect(top !== null && bottom !== null).toBe(true);
    if (top === null || bottom === null) return;
    expect(top.dec_deg).toBeGreaterThan(bottom.dec_deg);
  });
});
