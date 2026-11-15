// @vitest-environment jsdom
// The backend may answer /spectrum with only the points decoded by its soft
// deadline and complete the rest in the background: a fast answer with 1-2
// points is retried once, quietly, after SPECTRUM_RETRY_MS.
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  SPECTRUM_RETRY_MS,
  useSpectrum,
} from "../../src/components/spectrum/useSpectrum";
import type { SpectrumClient } from "../../src/components/spectrum/spectrumApi";
import type { SpectrumPoint, SpectrumResponse } from "../../src/components/spectrum/types";

function spectrumWith(n: number): SpectrumResponse {
  const points: SpectrumPoint[] = [];
  for (let i = 0; i < n; i++) {
    points.push({
      wavelength_um: 1 + i,
      value: 10 + i,
      sigma: 0.5,
      obs_mjd: 60200 + i,
      detector: 1,
    });
  }
  return { ra_deg: 1, dec_deg: 2, unit: "MJy/sr", points };
}

describe("useSpectrum aperture", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  // f11: the default aperture is 5 px — measured on Barnard's Star, the
  // old 3 px default missed the moving star between epochs and the values
  // jumped 165-1085 MJy/sr; 5 px is smooth.
  it("requests the 5 px aperture by default", async () => {
    let seen: number | null = null;
    const client: SpectrumClient = {
      spectrum: (_ra, _dec, aperturePx) => {
        seen = aperturePx;
        return Promise.resolve(spectrumWith(3));
      },
    };
    const { result } = renderHook(() => useSpectrum(client));
    act(() => result.current.requestAt(10, 20));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(result.current.aperturePx).toBe(5);
    expect(seen).toBe(5);
  });

  it("still honours a manual aperture choice", async () => {
    let seen: number | null = null;
    const client: SpectrumClient = {
      spectrum: (_ra, _dec, aperturePx) => {
        seen = aperturePx;
        return Promise.resolve(spectrumWith(3));
      },
    };
    const { result } = renderHook(() => useSpectrum(client));
    act(() => result.current.requestAt(10, 20));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    act(() => result.current.setAperture(3));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(seen).toBe(3);
    expect(result.current.aperturePx).toBe(3);
  });
});

describe("useSpectrum partial-answer retry", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it("shows a fast 1-2 point answer and retries once after 5 s", async () => {
    let calls = 0;
    const answers = [spectrumWith(2), spectrumWith(9)];
    const client: SpectrumClient = {
      spectrum: () => {
        calls += 1;
        return Promise.resolve(answers.shift() ?? spectrumWith(9));
      },
    };
    const { result } = renderHook(() => useSpectrum(client));
    act(() => result.current.requestAt(10, 20));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(result.current.status).toBe("success");
    expect(result.current.data?.points).toHaveLength(2);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(SPECTRUM_RETRY_MS);
    });
    expect(calls).toBe(2);
    expect(result.current.data?.points).toHaveLength(9);
    // No third request: the retry fires once.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(SPECTRUM_RETRY_MS * 3);
    });
    expect(calls).toBe(2);
  });

  it("does not retry a multi-point answer", async () => {
    let calls = 0;
    const client: SpectrumClient = {
      spectrum: () => {
        calls += 1;
        return Promise.resolve(spectrumWith(6));
      },
    };
    const { result } = renderHook(() => useSpectrum(client));
    act(() => result.current.requestAt(10, 20));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(SPECTRUM_RETRY_MS * 3);
    });
    expect(calls).toBe(1);
    expect(result.current.data?.points).toHaveLength(6);
  });

  it("keeps the partial spectrum when the retry fails", async () => {
    let calls = 0;
    const client: SpectrumClient = {
      spectrum: () => {
        calls += 1;
        return calls === 1
          ? Promise.resolve(spectrumWith(1))
          : Promise.reject(new Error("upstream"));
      },
    };
    const { result } = renderHook(() => useSpectrum(client));
    act(() => result.current.requestAt(10, 20));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(SPECTRUM_RETRY_MS);
    });
    expect(result.current.status).toBe("success");
    expect(result.current.data?.points).toHaveLength(1);
    expect(result.current.error).toBeNull();
  });

  it("a new click cancels a pending retry", async () => {
    let calls = 0;
    const client: SpectrumClient = {
      spectrum: () => {
        calls += 1;
        return Promise.resolve(spectrumWith(1));
      },
    };
    const { result } = renderHook(() => useSpectrum(client));
    act(() => result.current.requestAt(10, 20));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    act(() => result.current.requestAt(30, 40));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(SPECTRUM_RETRY_MS * 2);
    });
    // 2 initial requests + at most the retry of the second one.
    expect(calls).toBeLessThanOrEqual(3);
    expect(result.current.raDeg).toBe(30);
  });
});
