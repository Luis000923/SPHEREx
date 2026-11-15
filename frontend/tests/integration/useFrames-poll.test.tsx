// @vitest-environment jsdom
// The backend answers at its soft deadline with the cutouts decoded so far
// and keeps downloading; useFrames must ask again and show the growing
// series, retry a transient 502 instead of showing an error at once, and
// never retry a 404/400. Mock data simulates the same partial-first rhythm.
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useFrames } from "../../src/hooks/useFrames";
import { makeMockFramesResponse } from "../../src/lib/mock";
import {
  MAX_UPSTREAM_RETRIES,
  POLL_DELAY_MS,
  RETRY_DELAY_MAX_MS,
} from "../../src/lib/poll";

const params = { ra: "133.795", dec: "-7.245", fov: 0.05, mode: "raw" as const };

function partial(count: number, total = 24) {
  const full = makeMockFramesResponse({ count: total });
  return {
    ...full,
    frames: full.frames.slice(0, count),
    truncated: true,
    total_observations: 271,
  };
}

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("useFrames follow-up polling", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("re-requests a partial raw series until the 24 default frames arrive", async () => {
    const answers = [partial(3), partial(10), partial(24)];
    const fetchMock = vi.fn(() => Promise.resolve(jsonResponse(answers.shift())));
    vi.stubGlobal("fetch", fetchMock);
    const { result } = renderHook(() => useFrames("", params));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(result.current.frames).toHaveLength(3);
    expect(result.current.pending).toBe(true);
    expect(result.current.expected).toBe(24);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(POLL_DELAY_MS);
    });
    expect(result.current.frames).toHaveLength(10);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(POLL_DELAY_MS);
    });
    expect(result.current.frames).toHaveLength(24);
    expect(result.current.pending).toBe(false);
    expect(result.current.partial).toBe(false);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(POLL_DELAY_MS * 3);
    });
    expect(fetchMock).toHaveBeenCalledTimes(3);
  });

  it("polls a partial raw series even when truncated is false", async () => {
    const small = (count: number) => {
      const full = makeMockFramesResponse({ count: 10 });
      return {
        ...full,
        frames: full.frames.slice(0, count),
        truncated: false,
        total_observations: 10,
      };
    };
    const answers = [small(4), small(10)];
    const fetchMock = vi.fn(() => Promise.resolve(jsonResponse(answers.shift())));
    vi.stubGlobal("fetch", fetchMock);
    const { result } = renderHook(() => useFrames("", params));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(result.current.frames).toHaveLength(4);
    expect(result.current.expected).toBe(10);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(POLL_DELAY_MS);
    });
    expect(result.current.frames).toHaveLength(10);
    expect(result.current.pending).toBe(false);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  // f11: WISE 0855 ends at 22 of 24 on fully baked data — the answers
  // stop growing because the archive cannot supply the rest. That is the
  // calm "settled" note, not the retryable "partial" warning.
  it("settles calmly when answers stop growing below the expected count", async () => {
    const fetchMock = vi.fn(() => Promise.resolve(jsonResponse(partial(12))));
    vi.stubGlobal("fetch", fetchMock);
    const { result } = renderHook(() => useFrames("", params));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    // Two unchanged answers exhaust the stale budget.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(POLL_DELAY_MS * 2);
    });
    expect(result.current.pending).toBe(false);
    expect(result.current.partial).toBe(false);
    expect(result.current.settled).toBe(true);
    expect(result.current.frames).toHaveLength(12);
    expect(result.current.expected).toBe(24);
  });

  // The warning "partial" state is reserved for stops that a manual retry
  // can still help: the total time budget or a terminal error.
  it("stays a retryable partial when the time budget runs out mid-growth", async () => {
    // Every answer adds one frame but never reaches 24, so polling hits
    // POLL_BUDGET_MS before the stale check ever trips.
    const fetchMock = vi.fn(() => {
      const count = fetchMock.mock.calls.length;
      return Promise.resolve(jsonResponse(partial(Math.min(2 + count, 23))));
    });
    vi.stubGlobal("fetch", fetchMock);
    const { result } = renderHook(() => useFrames("", params));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(100_000);
    });
    expect(result.current.pending).toBe(false);
    expect(result.current.settled).toBe(false);
    expect(result.current.partial).toBe(true);
    expect(result.current.frames.length).toBeLessThan(24);
  });

  it("retries upstream_unavailable before surfacing an error", async () => {
    const busy = { error: { code: "upstream_unavailable", message: "busy" } };
    const answers = [jsonResponse(busy, 502), jsonResponse(partial(24))];
    vi.stubGlobal(
      "fetch",
      vi.fn(() => Promise.resolve(answers.shift() ?? jsonResponse(busy, 502))),
    );
    const { result } = renderHook(() => useFrames("", params));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(result.current.status).toBe("loading");
    expect(result.current.pending).toBe(true);
    expect(result.current.errorCode).toBe("upstream_unavailable");
    await act(async () => {
      await vi.advanceTimersByTimeAsync(RETRY_DELAY_MAX_MS);
    });
    expect(result.current.status).toBe("success");
    expect(result.current.frames).toHaveLength(24);
  });

  it("gives up after the upstream retry budget and shows the error", async () => {
    const busy = { error: { code: "upstream_unavailable", message: "busy" } };
    const fetchMock = vi.fn(() => Promise.resolve(jsonResponse(busy, 502)));
    vi.stubGlobal("fetch", fetchMock);
    const { result } = renderHook(() => useFrames("", params));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    // 1 initial request + MAX_UPSTREAM_RETRIES retries (backoff capped).
    await act(async () => {
      await vi.advanceTimersByTimeAsync(120_000);
    });
    expect(result.current.status).toBe("error");
    expect(result.current.errorCode).toBe("upstream_unavailable");
    expect(fetchMock).toHaveBeenCalledTimes(1 + MAX_UPSTREAM_RETRIES);
  });

  it("keeps fetched frames when a follow-up request fails permanently", async () => {
    const nf = { error: { code: "internal", message: "boom" } };
    const answers = [jsonResponse(partial(8)), jsonResponse(nf, 500)];
    vi.stubGlobal(
      "fetch",
      vi.fn(() => Promise.resolve(answers.shift() ?? jsonResponse(nf, 500))),
    );
    const { result } = renderHook(() => useFrames("", params));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(result.current.frames).toHaveLength(8);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(POLL_DELAY_MS);
    });
    // "internal" is never retried: the partial series stays on screen.
    expect(result.current.status).toBe("success");
    expect(result.current.frames).toHaveLength(8);
    expect(result.current.pending).toBe(false);
    expect(result.current.partial).toBe(true);
  });

  it("does not retry a 404", async () => {
    const nf = { error: { code: "not_found", message: "none" } };
    const fetchMock = vi.fn(() => Promise.resolve(jsonResponse(nf, 404)));
    vi.stubGlobal("fetch", fetchMock);
    const { result } = renderHook(() => useFrames("", params));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(POLL_DELAY_MS * 2);
    });
    expect(result.current.status).toBe("error");
    expect(result.current.errorCode).toBe("not_found");
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("omits max_frames from the request URL when unset", async () => {
    const fetchMock = vi.fn<typeof fetch>(() =>
      Promise.resolve(jsonResponse(partial(24))),
    );
    vi.stubGlobal("fetch", fetchMock);
    renderHook(() => useFrames("", params));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    const url = String(fetchMock.mock.calls[0]?.[0]);
    expect(url).not.toContain("max_frames");
  });
});

describe("useFrames mock simulation", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it("answers partial first (pending) and completes after the mock delay", async () => {
    const mock = makeMockFramesResponse({ count: 12 });
    const { result } = renderHook(() => useFrames(null, params, mock, 500));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(result.current.frames).toHaveLength(4);
    expect(result.current.pending).toBe(true);
    expect(result.current.expected).toBe(12);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });
    expect(result.current.frames).toHaveLength(12);
    expect(result.current.pending).toBe(false);
  });

  it("serves the full series at once when it cannot be split", async () => {
    const mock = makeMockFramesResponse({ count: 1 });
    const { result } = renderHook(() => useFrames(null, params, mock, 500));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(result.current.frames).toHaveLength(1);
    expect(result.current.pending).toBe(false);
  });

  it("completes a small series after the mock delay too", async () => {
    const mock = makeMockFramesResponse({ count: 3 });
    const { result } = renderHook(() => useFrames(null, params, mock, 500));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(result.current.pending).toBe(true);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });
    expect(result.current.frames).toHaveLength(3);
    expect(result.current.pending).toBe(false);
  });
});
