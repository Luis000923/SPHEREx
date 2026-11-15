// @vitest-environment jsdom
// f12 regression: uPlot defers its first draw (_commit) to a microtask, so
// it can run after the panel has unmounted or the test has finished.
// jsdom has no canvas backend; tests/setup.ts gives every canvas a no-op
// 2D context. Without that stub the late commit crashed on
// ctx.clearRect as an uncaught microtask error — the flake that turned
// the CI frontend job red while every test passed. This test mounts the
// real panel, waits until the chart exists (constructor ran, commit
// queued), unmounts, flushes the queue and asserts nothing escapes.
import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { SpectrumPanel } from "../../src/components/spectrum/SpectrumPanel";
import { makeMockSpectrum } from "../../src/components/spectrum/mockSpectrum";

// jsdom has no matchMedia; the panel and uPlot both call it.
function stubMatchMedia(): void {
  window.matchMedia = ((query: string) => ({
    matches: true,
    media: query,
    onchange: null,
    addListener() {},
    removeListener() {},
    addEventListener() {},
    removeEventListener() {},
    dispatchEvent: () => false,
  })) as unknown as typeof window.matchMedia;
}

// Structural typing: @types/node is not a dependency, but vitest workers
// always provide globalThis.process.
interface ProcessLike {
  on(event: string, listener: (err: unknown) => void): void;
  off(event: string, listener: (err: unknown) => void): void;
}

beforeEach(stubMatchMedia);
afterEach(cleanup);

describe("SpectrumPanel chart lifecycle", () => {
  it("a uPlot commit queued before unmount produces no unhandled error", async () => {
    const errors: unknown[] = [];
    const onError = (err: unknown) => {
      errors.push(err);
    };
    const proc = (globalThis as { process?: ProcessLike }).process;
    proc?.on("uncaughtException", onError);
    proc?.on("unhandledRejection", onError);
    try {
      const view = render(
        <SpectrumPanel
          open
          status="success"
          spectrum={makeMockSpectrum(10, -5)}
          error={null}
          raDeg={10}
          decDeg={-5}
          aperturePx={3}
          onApertureChange={() => {}}
          onClose={() => {}}
        />,
      );
      // The uPlot root node is appended synchronously by the constructor,
      // which also queues _commit on a microtask — waiting for the node
      // guarantees a commit is in flight before the unmount.
      await waitFor(() =>
        expect(document.querySelector(".uplot")).not.toBeNull(),
      );
      view.unmount();
      // Flush microtasks and a macrotask so the queued commit runs now.
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(errors).toEqual([]);
    } finally {
      proc?.off("uncaughtException", onError);
      proc?.off("unhandledRejection", onError);
    }
  });
});
