// Canvas click -> sky position. The contract's Wcs is a linear approximation
// of the celestial WCS (TAN): crpix is 1-based, cd is deg/pixel. Pure
// functions so they can be unit-tested without a DOM.
import type { Wcs } from "../../lib/types";

export interface SkyCoord {
  ra_deg: number;
  dec_deg: number;
}

const DEG = Math.PI / 180;

// Maps a 0-based pixel coordinate (x right, y down, as in the row-major pixel
// buffer and canvas layout) to ICRS degrees through the gnomonic (TAN)
// projection the contract's Wcs describes: CD gives intermediate world
// coordinates (xi, eta) in degrees, which are deprojected about crval. The
// earlier linear form (ra = crval + cd * dpix) ignored the 1/cos(dec) factor
// and misplaced clicks by 2.5x in RA at dec +66.75 (R Draconis). Returns null
// for non-finite input.
export function pixelToSky(wcs: Wcs, x: number, y: number): SkyCoord | null {
  if (!Number.isFinite(x) || !Number.isFinite(y)) return null;
  const u = x + 1 - wcs.crpix[0]; // FITS pixels are 1-based
  const v = y + 1 - wcs.crpix[1];
  const row0 = wcs.cd[0];
  const row1 = wcs.cd[1];
  if (row0 === undefined || row1 === undefined) return null;
  const xi = (row0[0] * u + row0[1] * v) * DEG;
  const eta = (row1[0] * u + row1[1] * v) * DEG;
  const ra0 = wcs.crval[0] * DEG;
  const dec0 = wcs.crval[1] * DEG;
  const den = Math.cos(dec0) - eta * Math.sin(dec0);
  const ra = ra0 + Math.atan2(xi, den);
  const dec = Math.atan2(
    Math.sin(dec0) + eta * Math.cos(dec0),
    Math.hypot(xi, den),
  );
  const raDeg = ra / DEG;
  const decDeg = dec / DEG;
  if (!Number.isFinite(raDeg) || !Number.isFinite(decDeg)) return null;
  return { ra_deg: ((raDeg % 360) + 360) % 360, dec_deg: decDeg };
}

export interface RectLike {
  left: number;
  top: number;
  width: number;
  height: number;
}

// Converts a client-space point (e.g. MouseEvent.clientX/Y) to a 0-based
// pixel coordinate in an image of `width`x`height` shown inside `rect`
// with `object-fit: contain` (uniform scale, centred).
// Returns null when the rect is degenerate or inputs are non-finite.
export function clientPointToPixel(
  rect: RectLike,
  clientX: number,
  clientY: number,
  width: number,
  height: number,
): { x: number; y: number } | null {
  if (
    !Number.isFinite(clientX) ||
    !Number.isFinite(clientY) ||
    !Number.isFinite(rect.width) ||
    !Number.isFinite(rect.height) ||
    rect.width <= 0 ||
    rect.height <= 0 ||
    width <= 0 ||
    height <= 0
  ) {
    return null;
  }
  // The canvas is styled `object-fit: contain`: the image is scaled
  // uniformly and centred, leaving letterbox bars on the long axis. Clicks
  // on the bars are outside the image.
  const scale = Math.min(rect.width / width, rect.height / height);
  const offX = (rect.width - width * scale) / 2;
  const offY = (rect.height - height * scale) / 2;
  const x = (clientX - rect.left - offX) / scale;
  const y = (clientY - rect.top - offY) / scale;
  if (x < 0 || y < 0 || x > width || y > height) return null;
  return { x, y };
}

// Full pipeline for a click on the rendered cutout canvas. `canvas` is the
// element that was clicked (must be the cutout canvas, not an overlay).
export function canvasPickToSky(
  canvas: HTMLCanvasElement,
  clientX: number,
  clientY: number,
  wcs: Wcs,
  width: number,
  height: number,
): SkyCoord | null {
  const px = clientPointToPixel(
    canvas.getBoundingClientRect(),
    clientX,
    clientY,
    width,
    height,
  );
  if (px === null) return null;
  return pixelToSky(wcs, px.x, px.y);
}
