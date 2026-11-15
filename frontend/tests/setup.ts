// Vitest setup (registered via test.setupFiles in vite.config.ts).
//
// jsdom ships no canvas raster backend: getContext("2d") returns null
// after logging "Not implemented". uPlot stores that null as its drawing
// context and its deferred _commit() — queued on a microtask by the
// constructor — then crashes on ctx.clearRect. Depending on timing the
// throw landed after test teardown, surfacing as a flaky unhandled error
// in CI: "TypeError: Cannot read properties of null (reading 'clearRect')"
// from uPlot.cjs.js. This stub hands every canvas a minimal no-op 2D
// context so a late commit is harmless. Non-2D contexts still return
// null, so the app's WebGL2 -> Canvas2D -> Null renderer fallback logic
// is unchanged.

function stubImageData(width: number, height: number): ImageData {
  const w = Math.max(0, Math.floor(width));
  const h = Math.max(0, Math.floor(height));
  return {
    width: w,
    height: h,
    data: new Uint8ClampedArray(w * h * 4),
    colorSpace: "srgb",
  };
}

// One fake context per canvas (a WeakMap keeps identity stable and lets
// the element be collected). Method bodies are irrelevant — every call is
// a no-op — but the few members code reads back (canvas, measureText,
// gradients, ImageData factories) must return well-formed values.
function stub2dContext(canvas: HTMLCanvasElement): CanvasRenderingContext2D {
  const gradient = { addColorStop: () => {} };
  const target: Record<PropertyKey, unknown> = {
    canvas,
    measureText: () => ({ width: 0 }),
    createLinearGradient: () => gradient,
    createRadialGradient: () => gradient,
    createConicGradient: () => gradient,
    createPattern: () => ({}),
    createImageData: (w: number, h: number) => stubImageData(w, h),
    getImageData: (_x: number, _y: number, w: number, h: number) =>
      stubImageData(w, h),
    getLineDash: () => [],
    isPointInPath: () => false,
    isPointInStroke: () => false,
  };
  return new Proxy(target, {
    // Anything not listed above resolves to a no-op function. Style
    // members (fillStyle, font, globalAlpha, ...) are only ever assigned,
    // so the set trap below absorbs them.
    get(t, prop) {
      return prop in t ? t[prop] : () => {};
    },
    set() {
      return true;
    },
  }) as unknown as CanvasRenderingContext2D;
}

// jsdom also lacks Path2D, which uPlot uses to build series paths; once
// the 2D context above lets _commit run, the next crash is a bare
// "ReferenceError: Path2D is not defined". A construct-trap Proxy makes
// every instance method a no-op.
if (typeof Path2D === "undefined") {
  (globalThis as Record<string, unknown>).Path2D = new Proxy(class {}, {
    construct: () => new Proxy({}, { get: () => () => {} }),
  });
}

if (typeof HTMLCanvasElement !== "undefined") {
  const contexts = new WeakMap<
    HTMLCanvasElement,
    CanvasRenderingContext2D
  >();
  HTMLCanvasElement.prototype.getContext = function (
    this: HTMLCanvasElement,
    kind: string,
  ) {
    if (kind !== "2d") {
      // webgl/webgl2/bitmaprenderer: report "unsupported" (null), like a
      // GPU-less browser, without jsdom's "Not implemented" console noise.
      return null;
    }
    let ctx = contexts.get(this);
    if (ctx === undefined) {
      ctx = stub2dContext(this);
      contexts.set(this, ctx);
    }
    return ctx;
  } as unknown as typeof HTMLCanvasElement.prototype.getContext;
}
