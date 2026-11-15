// Frame fetching with cancellation. Pass mockData to bypass the network
// entirely (development, tests, embeds without a backend).
//
// Partial answers are followed up automatically (lib/poll): the backend
// returns what it has at its soft deadline and keeps downloading the rest,
// so asking again a few seconds later yields more frames. `pending` is true
// while such a follow-up is scheduled; `partial` when polling gave up below
// the expected count. Mock data simulates the same rhythm (a partial answer
// first, the complete one `mockDelay` ms later) so the UI states can be
// exercised offline.
import {
  useEffect,
  useRef,
  useState,
  type Dispatch,
  type SetStateAction,
} from "react";
import { base64ToFloat32 } from "../lib/base64";
import { ApiError, fetchFrames, type FramesParams } from "../lib/api";
import { expectedFrames, nextPollDelay, stopReason } from "../lib/poll";
import { parseDecToDeg, parseRaToDeg } from "../lib/coords";
import type { DecodedFrame, FramesResponse } from "../lib/types";

export type FramesStatus = "idle" | "loading" | "success" | "error";

export interface FramesResult {
  status: FramesStatus;
  response: FramesResponse | null;
  frames: DecodedFrame[];
  error: string | null;
  /** Contract error code of the last failure, if any. */
  errorCode?: string | null;
  /** A follow-up request is scheduled (more frames may arrive). */
  pending?: boolean;
  /** Expected series length for a raw query, null when unknowable. */
  expected?: number | null;
  /**
   * Polling ended below the expected count on the time budget or an
   * error; a manual retry may still add frames (warning state).
   */
  partial?: boolean;
  /**
   * Polling ended below the expected count because the answers stopped
   * growing: the archive has nothing more to give right now (calm note,
   * not a warning).
   */
  settled?: boolean;
}

/** Default delay of the mock-mode "still downloading" phase. */
export const MOCK_PARTIAL_MS = 800;

function decodeAll(res: FramesResponse): DecodedFrame[] {
  const px = res.width * res.height;
  const out: DecodedFrame[] = [];
  for (const meta of res.frames) {
    try {
      const pixels = base64ToFloat32(meta.data_b64);
      if (pixels.length === px) out.push({ meta, pixels });
    } catch {
      // skip undecodable frames rather than failing the whole series
    }
  }
  return out;
}

const IDLE_RESULT: FramesResult = {
  status: "idle",
  response: null,
  frames: [],
  error: null,
};

/**
 * Drives the follow-up-request contract outside React: requests the same
 * URL while `nextPollDelay` says the series can still grow, at most one
 * request in flight. `alive()` gates every state write so a superseded or
 * unmounted effect can never resurrect. Returns the stop function.
 */
function startFramesPoll(
  apiBase: string,
  params: FramesParams,
  alive: () => boolean,
  emit: Dispatch<SetStateAction<FramesResult>>,
): () => void {
  const ctrl = new AbortController();
  let timer: ReturnType<typeof setTimeout> | undefined;
  let polls = 0;
  let stale = 0;
  let lastCount = -1;
  let upstreamRetries = 0;
  const started = Date.now();

  const schedule = (delay: number | null) => {
    if (delay === null || ctrl.signal.aborted) return;
    timer = setTimeout(() => {
      polls += 1;
      run();
    }, delay);
  };

  const run = () => {
    fetchFrames(apiBase, params, ctrl.signal)
      .then((res) => {
        if (!alive()) return;
        const frames = decodeAll(res);
        stale = frames.length > lastCount ? 0 : stale + 1;
        lastCount = Math.max(lastCount, frames.length);
        upstreamRetries = 0;
        const input = {
          polls,
          stalePolls: stale,
          upstreamRetries,
          elapsedMs: Date.now() - started,
          mode: params.mode,
          maxFrames: params.maxFrames,
          frames: frames.length,
          truncated: res.truncated,
          totalObservations: res.total_observations ?? null,
        };
        const delay = nextPollDelay(input);
        // Below the expected count, why polling ended decides the UI:
        // "stale" is a calm "this is all the archive has" note; the time
        // budget and terminal errors keep the retryable warning.
        const reason = delay === null ? stopReason(input) : null;
        const expected = expectedFrames(
          params.mode,
          params.maxFrames,
          res.total_observations ?? null,
        );
        const short = expected !== null && frames.length < expected;
        emit({
          status: "success",
          response: res,
          frames,
          error: null,
          errorCode: null,
          pending: delay !== null,
          expected,
          partial:
            short && (reason === "budget" || reason === "error"),
          settled: short && reason === "stale",
        });
        schedule(delay);
      })
      .catch((err: unknown) => {
        if (!alive() || ctrl.signal.aborted) return;
        const code = err instanceof ApiError ? err.code : "internal";
        if (code === "upstream_unavailable") upstreamRetries += 1;
        const delay = nextPollDelay({
          polls,
          stalePolls: stale,
          upstreamRetries,
          elapsedMs: Date.now() - started,
          mode: params.mode,
          maxFrames: params.maxFrames,
          errorCode: code,
        });
        if (delay !== null) {
          // Transient upstream failure: ask again. Frames already on
          // screen stay on screen.
          emit((prev) =>
            lastCount > 0
              ? { ...prev, pending: true }
              : { ...prev, status: "loading", errorCode: code, pending: true },
          );
          schedule(delay);
          return;
        }
        if (lastCount > 0) {
          // Partial series already shown: keep it, mark it partial —
          // a terminal failure is always the warning state, never the
          // calm "archive has no more" note.
          emit((prev) => ({
            ...prev,
            pending: false,
            settled: false,
            partial:
              prev.expected !== null &&
              prev.expected !== undefined &&
              prev.frames.length < prev.expected,
          }));
          return;
        }
        emit({
          status: "error",
          response: null,
          frames: [],
          error: err instanceof Error ? err.message : "request failed",
          errorCode: code,
          pending: false,
          partial: false,
          settled: false,
        });
      });
  };
  run();
  return () => {
    ctrl.abort();
    clearTimeout(timer);
  };
}

export function useFrames(
  apiBase: string | null,
  params: Omit<FramesParams, "ra" | "dec"> & { ra: string; dec: string },
  mockData?: FramesResponse,
  mockDelay: number = MOCK_PARTIAL_MS,
): FramesResult {
  const [result, setResult] = useState<FramesResult>(IDLE_RESULT);
  const seq = useRef(0);

  const raValid = parseRaToDeg(params.ra) !== null;
  const decValid = parseDecToDeg(params.dec) !== null;

  useEffect(() => {
    const id = ++seq.current;
    if (mockData !== undefined) {
      // Simulate the real contract: a partial answer first (pending), the
      // full series a beat later — the UI states are testable offline.
      const total = mockData.frames.length;
      const head = Math.max(1, Math.ceil(total / 3));
      const full: FramesResult = {
        status: "success",
        response: mockData,
        frames: decodeAll(mockData),
        error: null,
        errorCode: null,
        pending: false,
        expected: null,
        partial: false,
        settled: false,
      };
      if (head >= total || mockDelay <= 0) {
        setResult(full);
        return;
      }
      const sliced: FramesResponse = {
        ...mockData,
        frames: mockData.frames.slice(0, head),
        truncated: true,
        total_observations: total,
      };
      setResult({
        status: "success",
        response: sliced,
        frames: decodeAll(sliced),
        error: null,
        errorCode: null,
        pending: true,
        expected: expectedFrames(
          params.mode,
          params.maxFrames,
          sliced.total_observations ?? null,
        ),
        partial: false,
        settled: false,
      });
      const timer = setTimeout(() => {
        if (seq.current === id) setResult(full);
      }, mockDelay);
      return () => clearTimeout(timer);
    }
    if (apiBase === null || !raValid || !decValid) {
      setResult(IDLE_RESULT);
      return;
    }
    setResult((prev) => ({
      ...prev,
      status: "loading",
      error: null,
      errorCode: null,
      pending: false,
      partial: false,
      settled: false,
    }));
    return startFramesPoll(apiBase, params, () => seq.current === id, setResult);
  }, [
    apiBase,
    params.ra,
    params.dec,
    params.fov,
    params.mode,
    params.detector,
    params.maxFrames,
    params.subtractZodi,
    mockData,
    mockDelay,
    raValid,
    decValid,
  ]);

  return result;
}
