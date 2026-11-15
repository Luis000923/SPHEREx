// f10 regression: playback at 30 fps ran at ~20 fps on a 60 Hz display
// because the clock zeroed its accumulator on every step (and restarted with
// each frame), so every step cost three 16.7 ms ticks instead of two.
import { describe, expect, it } from "vitest";
import { advancePlayClock } from "../../src/lib/playClock";

function simulate(fps: number, hz: number, seconds: number): number {
  const dt = 1000 / hz;
  let acc = 0;
  let steps = 0;
  for (let i = 0; i < hz * seconds; i++) {
    const r = advancePlayClock(acc, dt, fps);
    acc = r.acc;
    steps += r.steps;
  }
  return steps;
}

describe("advancePlayClock", () => {
  it("keeps the requested rate at 30 fps on a 60 Hz display", () => {
    expect(simulate(30, 60, 5)).toBeGreaterThanOrEqual(148);
    expect(simulate(30, 60, 5)).toBeLessThanOrEqual(150);
  });

  it("keeps 5 fps on 60 Hz and 144 Hz displays", () => {
    expect(simulate(5, 60, 10)).toBe(50);
    expect(simulate(5, 144, 10)).toBeGreaterThanOrEqual(49);
    expect(simulate(5, 144, 10)).toBeLessThanOrEqual(50);
  });

  it("advances at most one frame per tick after a long stall", () => {
    const r = advancePlayClock(0, 5000, 5);
    expect(r.steps).toBe(1);
    expect(r.acc).toBeLessThan(1000 / 5);
  });

  it("ignores non-finite or negative deltas", () => {
    expect(advancePlayClock(10, NaN, 5)).toEqual({ acc: 10, steps: 0 });
    expect(advancePlayClock(10, -5, 5)).toEqual({ acc: 10, steps: 0 });
  });
});
