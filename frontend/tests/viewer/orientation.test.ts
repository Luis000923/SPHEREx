// @vitest-environment jsdom
// Orientation contract: the backend sends row-major pixels with row 0 =
// north (its WCS has cd22 < 0). The 2D canvas path must draw data row 0 at
// the TOP of the canvas, and the click->sky mapping must agree (a click on
// the top edge returns a larger Dec than one on the bottom edge). Uses an
// asymmetric synthetic frame so a north/south flip cannot hide.
import { describe, expect, it } from "vitest";
import { Canvas2DRenderer } from "../../src/lib/gl";
import { pixelToSky } from "../../src/components/spectrum/pick";
import type { Wcs } from "../../src/lib/types";

interface FakeImage {
  data: Uint8ClampedArray;
  width: number;
  height: number;
}

// jsdom canvases have no raster context; a recording stub stands in for
// CanvasRenderingContext2D.
function fakeCanvas(): {
  canvas: HTMLCanvasElement;
  drawn: FakeImage[];
} {
  const drawn: FakeImage[] = [];
  const ctx = {
    createImageData: (w: number, h: number): FakeImage => ({
      data: new Uint8ClampedArray(w * h * 4),
      width: w,
      height: h,
    }),
    putImageData: (img: FakeImage) => {
      drawn.push(img);
    },
  };
  const canvas = {
    getContext: (kind: string) => (kind === "2d" ? ctx : null),
    width: 0,
    height: 0,
  } as unknown as HTMLCanvasElement;
  return { canvas, drawn };
}

function lum(img: FakeImage, x: number, y: number): number {
  const o = (y * img.width + x) * 4;
  return ((img.data[o] ?? 0) + (img.data[o + 1] ?? 0) + (img.data[o + 2] ?? 0)) / 3;
}

describe("frame orientation (north up)", () => {
  const w = 4;
  const h = 4;
  // Bright source in data row 0, everything else dark.
  const pixels = new Float32Array(w * h);
  pixels[1 * w + 1] = 0; // keep neighbours unambiguous
  pixels[0 * w + 1] = 100;
  // Backend-style WCS: cd22 < 0 -> row 0 is north.
  const wcs: Wcs = {
    crpix: [w / 2, h / 2],
    crval: [200, 40],
    cd: [
      [-0.001, 0],
      [0, -0.001],
    ],
  };

  it("draws data row 0 at the top of the canvas", () => {
    const { canvas, drawn } = fakeCanvas();
    const r = Canvas2DRenderer.create(canvas);
    expect(r).not.toBeNull();
    if (r === null) return;
    r.upload(pixels, w, h);
    r.draw({ lo: 0, hi: 100, stretch: "linear", cmap: "grayscale" });
    expect(drawn).toHaveLength(1);
    const img = drawn[0];
    if (img === undefined) return;
    // putImageData at (0,0): image row 0 is the canvas top edge.
    const top = lum(img, 1, 0);
    const bottom = lum(img, 1, h - 1);
    expect(top).toBeGreaterThan(bottom);
    expect(top - bottom).toBeGreaterThan(100);
  });

  it("maps a click on the top edge to a larger Dec than the bottom edge", () => {
    const top = pixelToSky(wcs, 1, 0);
    const bottom = pixelToSky(wcs, 1, h - 1);
    expect(top !== null && bottom !== null).toBe(true);
    if (top === null || bottom === null) return;
    expect(top.dec_deg).toBeGreaterThan(bottom.dec_deg);
    // And the bright pixel's own row resolves north of the frame centre.
    const bright = pixelToSky(wcs, 1, 0);
    expect(bright !== null && bright.dec_deg > wcs.crval[1]).toBe(true);
  });
});
