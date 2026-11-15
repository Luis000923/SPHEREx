// When to ask the backend again for the same /frames query.
//
// The backend answers within its soft deadline with whatever cutouts have
// decoded and keeps fetching the rest in the background (filling its
// cache), so the same request a few seconds later returns more frames. A
// 502 upstream_unavailable with nothing decoded yet behaves the same way.
// Pure functions so they can be unit-tested without timers or a DOM.

/** Contract default for `max_frames`; also the selection the server pre-warms. */
export const DEFAULT_MAX_FRAMES = 24;
/** Delay between follow-up requests while a raw series is incomplete. */
export const POLL_DELAY_MS = 6000;
/**
 * Follow-up cadence for the combining modes (diff/coadd/stack). Their
 * output count is unknowable, so there is nothing to wait for once the
 * answer stops growing: a shorter interval settles a stalled series in
 * ~8 s (two unchanged answers apart) instead of keeping "Loading more
 * frames…" on screen while re-downloading the same data.
 */
export const POLL_DELAY_COMBINING_MS = 4000;
/** Total polling budget per query (~90 s), then whatever arrived is shown. */
export const POLL_BUDGET_MS = 90_000;
/** Stop after this many consecutive answers that added no new frames. */
export const MAX_STALE_POLLS = 2;
/** Automatic retries for a 502 upstream_unavailable (exponential backoff). */
export const MAX_UPSTREAM_RETRIES = 4;
export const RETRY_DELAY_MS = 4000;
export const RETRY_BACKOFF = 1.8;
export const RETRY_DELAY_MAX_MS = 20_000;

/**
 * Frame count a raw-mode series is expected to reach: min(max_frames,
 * total_observations), or just the cap when the backend did not report a
 * total. Null for the combining modes (diff/coadd/stack merge inputs into
 * an unpredictable number of output frames, so no target exists).
 */
export function expectedFrames(
  mode: string,
  maxFrames: number | undefined,
  totalObservations: number | null,
): number | null {
  if (mode !== "raw") return null;
  const cap = maxFrames ?? DEFAULT_MAX_FRAMES;
  return totalObservations === null ? cap : Math.min(cap, totalObservations);
}

export interface PollInput {
  /** Follow-up requests already made for this query. */
  polls: number;
  /** Answers in a row that returned no new frames. */
  stalePolls: number;
  /** Consecutive 502 upstream_unavailable retries already scheduled. */
  upstreamRetries: number;
  /** Milliseconds since the first request of this query. */
  elapsedMs: number;
  mode: string;
  /** Requested `max_frames` (undefined = contract default). */
  maxFrames: number | undefined;
  /** Last answer: frame count and totals, or an error code. */
  frames?: number;
  truncated?: boolean;
  totalObservations?: number | null;
  errorCode?: string;
}

/**
 * Why a query stopped polling, or null while a follow-up is still due.
 * Mirrors nextPollDelay's checks in the same order, so the pair always
 * agrees: "complete" = the expected count arrived (or the stack image is
 * in); "stale" = the answers stopped growing below target, so the archive
 * has nothing more to give right now; "budget" = the total polling time
 * ran out mid-growth and a manual retry could still add frames; "error" =
 * the last answer was a terminal failure.
 */
export type StopReason = "complete" | "stale" | "budget" | "error";

export function stopReason(p: PollInput): StopReason | null {
  if (p.elapsedMs >= POLL_BUDGET_MS) return "budget";
  if (p.errorCode !== undefined) {
    // Only a transient upstream failure is worth retrying automatically.
    if (p.errorCode !== "upstream_unavailable") return "error";
    return p.upstreamRetries > MAX_UPSTREAM_RETRIES ? "error" : null;
  }
  if (p.frames === undefined) return null;
  const expected = expectedFrames(
    p.mode,
    p.maxFrames,
    p.totalObservations ?? null,
  );
  // The expected count is in: stop even when `truncated` stays true (it
  // also marks a series capped at max_frames, not only partial answers).
  if (expected !== null && p.frames >= expected) return "complete";
  // Stack merges every usable exposure into one deep image: once it is in,
  // the answer cannot grow (verified on all five presets with real data).
  if (p.mode === "stack" && p.frames >= 1) return "complete";
  // Anything below the target (or an unbounded combining-mode series):
  // keep polling while answers keep growing.
  return p.stalePolls < MAX_STALE_POLLS ? null : "stale";
}

/**
 * Milliseconds until the next follow-up request, or null to stop.
 *
 * Partial raw series are re-requested at a fixed POLL_DELAY_MS while they
 * can still grow; combining modes poll faster (POLL_DELAY_COMBINING_MS)
 * because a stalled answer is only worth one confirmation; a 502 retries
 * with exponential backoff (never any other error); everything stops at
 * the POLL_BUDGET_MS cap.
 */
export function nextPollDelay(p: PollInput): number | null {
  if (stopReason(p) !== null) return null;
  if (p.errorCode === "upstream_unavailable") {
    return Math.min(
      RETRY_DELAY_MS * RETRY_BACKOFF ** (p.upstreamRetries - 1),
      RETRY_DELAY_MAX_MS,
    );
  }
  if (p.frames === undefined) return null;
  const expected = expectedFrames(
    p.mode,
    p.maxFrames,
    p.totalObservations ?? null,
  );
  return expected === null ? POLL_DELAY_COMBINING_MS : POLL_DELAY_MS;
}
