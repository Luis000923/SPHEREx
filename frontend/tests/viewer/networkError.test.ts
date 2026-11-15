// f10 regression: with the backend down, the viewer and the spectrum panel
// showed the browser's raw "Failed to fetch". A network failure now becomes
// an ApiError with a calm, specific English message; aborts stay aborts.
import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiError, NETWORK_ERROR_MESSAGE, fetchFrames } from "../../src/lib/api";
import { createSpectrumClient } from "../../src/components/spectrum/spectrumApi";

afterEach(() => {
  vi.unstubAllGlobals();
});

const params = { ra: "10", dec: "2", fov: 0.05, mode: "raw" as const };

describe("network failures", () => {
  it("fetchFrames maps a fetch TypeError to a friendly ApiError", async () => {
    vi.stubGlobal("fetch", vi.fn(() => Promise.reject(new TypeError("Failed to fetch"))));
    const err = await fetchFrames("", params).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).code).toBe("network");
    expect((err as ApiError).message).toBe(NETWORK_ERROR_MESSAGE);
    expect(NETWORK_ERROR_MESSAGE).not.toMatch(/failed to fetch/i);
  });

  it("the spectrum client maps it the same way", async () => {
    vi.stubGlobal("fetch", vi.fn(() => Promise.reject(new TypeError("Failed to fetch"))));
    const err = await createSpectrumClient("")
      .spectrum(10, 2, 3)
      .catch((e: unknown) => e);
    expect((err as ApiError).message).toBe(NETWORK_ERROR_MESSAGE);
  });

  it("keeps AbortError untouched", async () => {
    const abort = new DOMException("aborted", "AbortError");
    vi.stubGlobal("fetch", vi.fn(() => Promise.reject(abort)));
    const err = await fetchFrames("", params).catch((e: unknown) => e);
    expect(err).toBe(abort);
  });
});
