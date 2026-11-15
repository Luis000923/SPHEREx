import { describe, expect, it } from "vitest";
import {
  DEFAULT_MAX_FRAMES,
  MAX_STALE_POLLS,
  MAX_UPSTREAM_RETRIES,
  POLL_BUDGET_MS,
  POLL_DELAY_COMBINING_MS,
  POLL_DELAY_MS,
  RETRY_DELAY_MAX_MS,
  RETRY_DELAY_MS,
  expectedFrames,
  nextPollDelay,
  stopReason,
  type PollInput,
} from "../../src/lib/poll";

const base: PollInput = {
  polls: 0,
  stalePolls: 0,
  upstreamRetries: 0,
  elapsedMs: 0,
  mode: "raw",
  maxFrames: undefined,
};

describe("expectedFrames", () => {
  it("is min(max_frames, total_observations) for raw", () => {
    expect(expectedFrames("raw", undefined, 271)).toBe(DEFAULT_MAX_FRAMES);
    expect(expectedFrames("raw", undefined, 10)).toBe(10);
    expect(expectedFrames("raw", 60, 100)).toBe(60);
    expect(expectedFrames("raw", undefined, null)).toBe(DEFAULT_MAX_FRAMES);
  });
  it("is null for the combining modes", () => {
    expect(expectedFrames("diff", undefined, 271)).toBeNull();
    expect(expectedFrames("coadd", undefined, 10)).toBeNull();
    expect(expectedFrames("stack", undefined, 10)).toBeNull();
  });
});

describe("nextPollDelay", () => {
  it("polls a raw series that has fewer frames than selected", () => {
    expect(
      nextPollDelay({ ...base, frames: 6, truncated: true, totalObservations: 271 }),
    ).toBe(POLL_DELAY_MS);
  });
  it("polls a partial raw series even when truncated is false", () => {
    // 10 observations, 6 decoded so far: the backend completes in the
    // background although nothing was capped.
    expect(
      nextPollDelay({
        ...base,
        frames: 6,
        truncated: false,
        totalObservations: 10,
      }),
    ).toBe(POLL_DELAY_MS);
  });
  it("stops once the default 24 are in, although truncated stays true", () => {
    expect(
      nextPollDelay({ ...base, frames: 24, truncated: true, totalObservations: 271 }),
    ).toBeNull();
  });
  it("uses the total when fewer observations exist than the cap", () => {
    expect(
      nextPollDelay({ ...base, frames: 9, truncated: true, totalObservations: 10 }),
    ).toBe(POLL_DELAY_MS);
    expect(
      nextPollDelay({ ...base, frames: 10, truncated: false, totalObservations: 10 }),
    ).toBeNull();
  });
  it("stops a stalled raw series after MAX_STALE_POLLS unchanged answers", () => {
    expect(
      nextPollDelay({
        ...base,
        stalePolls: MAX_STALE_POLLS,
        frames: 12,
        truncated: true,
        totalObservations: 271,
      }),
    ).toBeNull();
  });
  it("retries upstream_unavailable with backoff but not other errors", () => {
    expect(
      nextPollDelay({ ...base, errorCode: "upstream_unavailable", upstreamRetries: 1 }),
    ).toBe(RETRY_DELAY_MS);
    expect(
      nextPollDelay({ ...base, errorCode: "upstream_unavailable", upstreamRetries: 2 }),
    ).toBeGreaterThan(RETRY_DELAY_MS);
    expect(
      nextPollDelay({ ...base, errorCode: "upstream_unavailable", upstreamRetries: 4 }),
    ).toBeLessThanOrEqual(RETRY_DELAY_MAX_MS);
    expect(nextPollDelay({ ...base, errorCode: "not_found" })).toBeNull();
    expect(nextPollDelay({ ...base, errorCode: "invalid_parameter" })).toBeNull();
    expect(nextPollDelay({ ...base, errorCode: "rate_limited" })).toBeNull();
  });
  it("gives up retrying after MAX_UPSTREAM_RETRIES", () => {
    expect(
      nextPollDelay({
        ...base,
        errorCode: "upstream_unavailable",
        upstreamRetries: MAX_UPSTREAM_RETRIES + 1,
      }),
    ).toBeNull();
  });
  it("non-raw modes poll while the output grows, on the shorter cadence", () => {
    const diff = {
      ...base,
      mode: "diff",
      frames: 3,
      truncated: true,
      totalObservations: 271,
    };
    expect(nextPollDelay({ ...diff, stalePolls: 0 })).toBe(
      POLL_DELAY_COMBINING_MS,
    );
    expect(nextPollDelay({ ...diff, stalePolls: MAX_STALE_POLLS })).toBeNull();
  });

  // f11 regression: Diff/Co-add kept "Loading more frames… n so far" on
  // screen for ~12 s on fully baked data because every follow-up waited
  // POLL_DELAY_MS. The combining cadence is shorter so two stale answers
  // settle the series ~8 s after it stopped growing.
  it("a stalled combining-mode series settles about 8 s after its last growth", () => {
    const diff = {
      ...base,
      mode: "diff" as const,
      frames: 14,
      truncated: true,
      totalObservations: 280,
    };
    // Growing answer: poll again on the combining cadence.
    expect(nextPollDelay({ ...diff, stalePolls: 0 })).toBe(
      POLL_DELAY_COMBINING_MS,
    );
    // First unchanged answer: one confirming follow-up, then stop.
    expect(nextPollDelay({ ...diff, stalePolls: 1 })).toBe(
      POLL_DELAY_COMBINING_MS,
    );
    expect(nextPollDelay({ ...diff, stalePolls: MAX_STALE_POLLS })).toBeNull();
    // 2 follow-ups at POLL_DELAY_COMBINING_MS <= ~8 s of "loading" after
    // the answer stopped growing.
    expect(MAX_STALE_POLLS * POLL_DELAY_COMBINING_MS).toBeLessThanOrEqual(8000);
  });

  it("raw keeps the longer cadence while it can still grow", () => {
    expect(
      nextPollDelay({ ...base, frames: 6, truncated: true, totalObservations: 271 }),
    ).toBe(POLL_DELAY_MS);
    expect(POLL_DELAY_COMBINING_MS).toBeLessThan(POLL_DELAY_MS);
  });
  it("stops everything at the total polling budget", () => {
    expect(
      nextPollDelay({
        ...base,
        elapsedMs: POLL_BUDGET_MS,
        frames: 3,
        truncated: true,
        totalObservations: 271,
      }),
    ).toBeNull();
    expect(
      nextPollDelay({
        ...base,
        elapsedMs: POLL_BUDGET_MS + 1,
        errorCode: "upstream_unavailable",
        upstreamRetries: 1,
      }),
    ).toBeNull();
  });

  // f10 regression: Stack always answers with one combined image (all five
  // presets on real data). Polling it kept "Loading more frames… 1 so far"
  // on screen for ~13 s and re-downloaded the same answer twice.
  it("stops polling Stack once its single image arrived", () => {
    expect(
      nextPollDelay({ ...base, mode: "stack", frames: 1, truncated: true, totalObservations: 390 }),
    ).toBeNull();
  });

  it("still polls Stack while it has no image yet", () => {
    expect(
      nextPollDelay({ ...base, mode: "stack", frames: 0, truncated: true, totalObservations: 390 }),
    ).toBe(POLL_DELAY_COMBINING_MS);
  });
});

describe("stopReason", () => {
  it("is null while a follow-up is still due", () => {
    expect(
      stopReason({ ...base, frames: 6, truncated: true, totalObservations: 271 }),
    ).toBeNull();
    expect(
      stopReason({
        ...base,
        errorCode: "upstream_unavailable",
        upstreamRetries: 1,
      }),
    ).toBeNull();
  });

  it("is 'complete' at the expected count, and for a delivered stack", () => {
    expect(
      stopReason({ ...base, frames: 24, truncated: true, totalObservations: 271 }),
    ).toBe("complete");
    expect(
      stopReason({ ...base, mode: "stack", frames: 1, totalObservations: 390 }),
    ).toBe("complete");
  });

  // f11: WISE 0855 ends at 22 of 24 on baked data because two cutouts
  // always fail upstream — the answers stop growing. That stop is calm
  // ("the archive did not provide the rest"), not a retryable warning.
  it("is 'stale' when answers stopped growing below the expected count", () => {
    expect(
      stopReason({
        ...base,
        stalePolls: MAX_STALE_POLLS,
        frames: 22,
        truncated: true,
        totalObservations: 280,
      }),
    ).toBe("stale");
    expect(
      stopReason({ ...base, mode: "diff", stalePolls: MAX_STALE_POLLS, frames: 11 }),
    ).toBe("stale");
  });

  it("is 'budget' when the total polling time ran out mid-growth", () => {
    expect(
      stopReason({
        ...base,
        elapsedMs: POLL_BUDGET_MS,
        frames: 20,
        truncated: true,
        totalObservations: 271,
      }),
    ).toBe("budget");
  });

  it("is 'error' on a terminal failure or exhausted upstream retries", () => {
    expect(stopReason({ ...base, errorCode: "internal" })).toBe("error");
    expect(stopReason({ ...base, errorCode: "not_found" })).toBe("error");
    expect(
      stopReason({
        ...base,
        errorCode: "upstream_unavailable",
        upstreamRetries: MAX_UPSTREAM_RETRIES + 1,
      }),
    ).toBe("error");
  });
});
