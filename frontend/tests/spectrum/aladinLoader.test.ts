// @vitest-environment jsdom
// f10 regression: the pinned CDS URL (api/v3/3.8.2/aladin.js) answered 404
// on 2026-11-15, so the overlay always showed "could not load Aladin Lite".
// The loader now falls back to the same pinned version on jsDelivr (npm
// aladin-lite, an ES module whose default export is the `A` namespace).
import { describe, expect, it } from "vitest";
import {
  ALADIN_FALLBACK_URL,
  ALADIN_VERSION,
  loadAladinFrom,
  type AladinGlobal,
} from "../../src/components/overlays/aladin";

const fakeA = { init: Promise.resolve() } as unknown as AladinGlobal;

describe("Aladin Lite loader", () => {
  it("pins the fallback to the same version on jsDelivr", () => {
    expect(ALADIN_FALLBACK_URL).toBe(
      `https://cdn.jsdelivr.net/npm/aladin-lite@${ALADIN_VERSION}/dist/aladin.js`,
    );
  });

  it("uses the fallback source when the primary fails", async () => {
    const tried: string[] = [];
    const a = await loadAladinFrom([
      () => {
        tried.push("cds");
        return Promise.reject(new Error("404"));
      },
      () => {
        tried.push("jsdelivr");
        return Promise.resolve(fakeA);
      },
    ]);
    expect(a).toBe(fakeA);
    expect(tried).toEqual(["cds", "jsdelivr"]);
  });

  it("does not try the fallback when the primary works", async () => {
    const tried: string[] = [];
    await loadAladinFrom([
      () => {
        tried.push("cds");
        return Promise.resolve(fakeA);
      },
      () => {
        tried.push("jsdelivr");
        return Promise.resolve(fakeA);
      },
    ]);
    expect(tried).toEqual(["cds"]);
  });

  it("rejects when every source fails", async () => {
    await expect(
      loadAladinFrom([
        () => Promise.reject(new Error("a")),
        () => Promise.reject(new Error("b")),
      ]),
    ).rejects.toThrow("failed to load Aladin Lite");
  });
});
