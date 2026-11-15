// Playback clock for the requestAnimationFrame loop. Carries the remainder
// of the accumulator between steps (instead of zeroing it), so the frame
// rate matches the requested fps on any display refresh rate; a long stall
// (background tab) advances one frame, never a burst.

// Half a millisecond of slack absorbs float rounding of rAF timestamps
// (16.666 + 16.667 must count as one 33.333 ms period).
const EPSILON_MS = 0.5;

export function advancePlayClock(
  accMs: number,
  dtMs: number,
  fps: number,
): { acc: number; steps: number } {
  if (!Number.isFinite(dtMs) || dtMs < 0 || !(fps > 0)) {
    return { acc: accMs, steps: 0 };
  }
  const period = 1000 / fps;
  const acc = accMs + dtMs;
  if (acc + EPSILON_MS < period) return { acc, steps: 0 };
  const rest = acc - period;
  return { acc: rest > period ? 0 : Math.max(rest, 0), steps: 1 };
}
