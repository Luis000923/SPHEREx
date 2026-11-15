// Spectrum fetch state + the click-to-spectrum glue. `useSpectrum` owns the
// request lifecycle (abort on change, race guard); `canvasClickToSpectrum`
// turns a click on the time-lapse canvas into a request at that RA/Dec.
import { useCallback, useEffect, useRef, useState } from "react";
import type { DecodedFrame } from "../../lib/types";
import { canvasPickToSky } from "./pick";
import type { SpectrumClient } from "./spectrumApi";
import type { AperturePx, SpectrumResponse } from "./types";

export type SpectrumStatus = "idle" | "loading" | "success" | "error";

/**
 * The backend answers /spectrum inside its soft deadline with whatever
 * cutouts have decoded and keeps completing in the background. A response
 * with only 1-2 points that came back fast is almost certainly partial, so
 * the same request is issued once more a few seconds later.
 */
export const SPECTRUM_RETRY_MS = 5000;
export const SPECTRUM_PARTIAL_FAST_MS = 15_000;
export const SPECTRUM_PARTIAL_MAX_POINTS = 2;

export interface SpectrumState {
  status: SpectrumStatus;
  open: boolean;
  data: SpectrumResponse | null;
  error: string | null;
  raDeg: number | null;
  decDeg: number | null;
  aperturePx: AperturePx;
}

export interface SpectrumController extends SpectrumState {
  requestAt(raDeg: number, decDeg: number): void;
  setAperture(px: AperturePx): void;
  close(): void;
}

export function useSpectrum(client: SpectrumClient | null): SpectrumController {
  const [state, setState] = useState<SpectrumState>({
    status: "idle",
    open: false,
    data: null,
    error: null,
    raDeg: null,
    decDeg: null,
    // 5 px default: measured on Barnard's Star (f10 real data), a 3 px
    // aperture misses the star between epochs (values jumped 165-1085
    // MJy/sr band to band); 5 px follows the proper motion smoothly.
    aperturePx: 5,
  });
  const seq = useRef(0);
  const abort = useRef<AbortController | null>(null);
  const retryTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const stateRef = useRef(state);
  stateRef.current = state;

  useEffect(
    () => () => {
      abort.current?.abort();
      if (retryTimer.current !== null) clearTimeout(retryTimer.current);
    },
    [],
  );

  const requestAt = useCallback(
    (raDeg: number, decDeg: number, aperturePx?: AperturePx) => {
      const ap = aperturePx ?? stateRef.current.aperturePx;
      const id = ++seq.current;
      abort.current?.abort();
      if (retryTimer.current !== null) {
        clearTimeout(retryTimer.current);
        retryTimer.current = null;
      }
      if (client === null) {
        setState((s) => ({
          ...s,
          open: true,
          aperturePx: ap,
          raDeg,
          decDeg,
          data: null,
          status: "error",
          error: "Spectrum service unavailable",
        }));
        return;
      }
      setState((s) => ({
        ...s,
        open: true,
        aperturePx: ap,
        raDeg,
        decDeg,
        error: null,
        status: "loading",
      }));
      const ctrl = new AbortController();
      abort.current = ctrl;
      const t0 = Date.now();
      // `quiet` retries replace the data in place: the partial spectrum
      // already on screen never flips back to a loading state.
      const issue = (quiet: boolean) => {
        client
          .spectrum(raDeg, decDeg, ap, ctrl.signal)
          .then((data) => {
            if (seq.current !== id) return;
            setState((s) => ({ ...s, status: "success", data, error: null }));
            const fast = Date.now() - t0 < SPECTRUM_PARTIAL_FAST_MS;
            if (
              !quiet &&
              fast &&
              data.points.length >= 1 &&
              data.points.length <= SPECTRUM_PARTIAL_MAX_POINTS
            ) {
              retryTimer.current = setTimeout(() => {
                retryTimer.current = null;
                if (seq.current === id && !ctrl.signal.aborted) issue(true);
              }, SPECTRUM_RETRY_MS);
            }
          })
          .catch((err: unknown) => {
            if (seq.current !== id || ctrl.signal.aborted) return;
            if (quiet) return; // keep the partial spectrum on screen
            setState((s) => ({
              ...s,
              status: "error",
              data: null,
              error:
                err instanceof Error ? err.message : "Spectrum request failed",
            }));
          });
      };
      issue(false);
    },
    [client],
  );

  const setAperture = useCallback(
    (px: AperturePx) => {
      const s = stateRef.current;
      if (s.raDeg !== null && s.decDeg !== null && s.status !== "idle") {
        requestAt(s.raDeg, s.decDeg, px); // refetch at the same position
      } else {
        setState((prev) => ({ ...prev, aperturePx: px }));
      }
    },
    [requestAt],
  );

  const close = useCallback(() => {
    seq.current += 1; // drop in-flight results
    abort.current?.abort();
    if (retryTimer.current !== null) {
      clearTimeout(retryTimer.current);
      retryTimer.current = null;
    }
    setState((s) => ({ ...s, open: false }));
  }, []);

  return { ...state, requestAt, setAperture, close };
}

export interface CanvasPickContext {
  frame: DecodedFrame | null;
  width: number;
  height: number;
}

// Returns a click handler for the element that wraps the time-lapse canvas.
// Only clicks whose target is the cutout <canvas> are converted (overlay divs
// and controls are ignored). The resulting RA/Dec is passed to `onPick`.
export function canvasClickToSpectrum(
  ctx: CanvasPickContext,
  onPick: (raDeg: number, decDeg: number) => void,
): (e: React.MouseEvent<HTMLElement>) => void {
  return (e) => {
    const target = e.target;
    if (!(target instanceof HTMLCanvasElement) || ctx.frame === null) return;
    const sky = canvasPickToSky(
      target,
      e.clientX,
      e.clientY,
      ctx.frame.meta.wcs,
      ctx.width,
      ctx.height,
    );
    if (sky !== null) onPick(sky.ra_deg, sky.dec_deg);
  };
}
