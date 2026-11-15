// Time-lapse viewer: decodes float32 frames onto a WebGL2 R32F texture
// (2D canvas fallback), requestAnimationFrame playback, scrubber, blink
// comparator and a per-frame metadata overlay.
//
// Playback is imperative: the rAF loop advances a live index kept in a
// ref and paints the canvas + transport DOM directly, so no React render
// (and no history.replaceState, which used to run once per frame) happens
// while playing. The live index is committed to the URL once, on pause or
// on a natural stop. While paused, React renders drive everything as usual.
import { useCallback, useEffect, useRef, useState } from "react";
import type { DecodedFrame, Frame } from "../../lib/types";
import {
  frameInterval,
  type Interval,
  type StretchKind,
} from "../../lib/stretch";
import {
  isDiverging as isDivergingCmap,
  type ColormapName,
} from "../../lib/colormaps";
import { createFrameRenderer, type FrameRenderer } from "../../lib/gl";
import { mjdToIso } from "../../lib/coords";
import { FPS_MAX, FPS_MIN } from "../../lib/urlState";
import { advancePlayClock } from "../../lib/playClock";
import { useMediaQuery } from "../layout/useMediaQuery";
import "./viewer.css";

export interface TransportState {
  playing: boolean;
  frameIndex: number;
  fps: number;
  loop: boolean;
  blink: boolean;
}

export interface TimeLapseCanvasProps {
  frames: DecodedFrame[];
  width: number;
  height: number;
  unit?: string;
  stretch: StretchKind;
  cmap: ColormapName;
  transport: TransportState;
  onTransport: (patch: Partial<TransportState>) => void;
  embed?: boolean;
  /** Frames hold signed values (Diff mode): zero is drawn mid-range. */
  signed?: boolean;
}

// 16px stroke icon for the transport buttons (inline SVG, no icon font).
function Icon(props: { d: string; fill?: boolean }) {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d={props.d}
        fill={props.fill === true ? "currentColor" : "none"}
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function clampIdx(i: number, n: number): number {
  return Math.min(Math.max(i, 0), Math.max(n - 1, 0));
}

function frameAriaText(
  meta: Frame | undefined,
  shownIdx: number,
  total: number,
): string {
  return meta === undefined
    ? "No frames"
    : `Frame ${shownIdx + 1} of ${total}, detector ${meta.detector}, ${meta.wavelength_um.toFixed(2)} micrometers, observed ${mjdToIso(meta.obs_mjd_start)}`;
}

// Elements the playback loop rewrites directly while playing. The React
// tree shows the last committed index then; the imperative writes keep
// the real-time position on screen, and the commit on pause reconciles
// both without any flicker.
interface LiveDom {
  canvas: HTMLCanvasElement | null;
  sr: HTMLElement | null;
  count: HTMLElement | null;
  scrub: HTMLInputElement | null;
  mjd: HTMLElement | null;
  det: HTMLElement | null;
  flag: HTMLElement | null;
  blink: HTMLElement | null;
}

function syncLiveDom(
  refs: LiveDom,
  meta: Frame | undefined,
  shownIdx: number,
  index: number,
  total: number,
  phase: 0 | 1,
): void {
  const label = frameAriaText(meta, shownIdx, total);
  refs.canvas?.setAttribute("aria-label", label);
  if (refs.sr !== null) refs.sr.textContent = label;
  if (refs.count !== null) refs.count.textContent = String(shownIdx + 1);
  if (refs.scrub !== null) refs.scrub.value = String(index);
  if (refs.mjd !== null) {
    refs.mjd.textContent = mjdToIso(meta?.obs_mjd_start ?? NaN);
  }
  if (refs.det !== null) {
    refs.det.textContent =
      meta === undefined
        ? "D?"
        : `D${meta.detector} · ${meta.wavelength_um.toFixed(2)} µm`;
  }
  if (refs.flag !== null) {
    const names = meta?.flags ?? [];
    refs.flag.textContent = names.join(", ");
    refs.flag.hidden = names.length === 0;
  }
  if (refs.blink !== null) refs.blink.textContent = phase === 0 ? "A" : "B";
}

// Mutable playback state read by the rAF loop. Assigned once per render so
// the loop always sees the latest props without being torn down.
interface LiveState {
  playing: boolean;
  index: number;
  phase: 0 | 1;
  frames: DecodedFrame[];
  width: number;
  height: number;
  stretch: StretchKind;
  cmap: ColormapName;
  signed: boolean;
  blink: boolean;
  anchor: number;
  loop: boolean;
  reduceMotion: boolean;
}

export function TimeLapseCanvas(props: TimeLapseCanvasProps) {
  const { frames, transport, onTransport, embed = false, signed = false } = props;
  const n = frames.length;
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const rendererRef = useRef<FrameRenderer | null>(null);
  const lastPixelsRef = useRef<Float32Array | null>(null);
  const [blinkA, setBlinkA] = useState(0);
  const [blinkPhase, setBlinkPhase] = useState<0 | 1>(0);

  // Imperative-DOM targets (see LiveDom).
  const srRef = useRef<HTMLSpanElement | null>(null);
  const countRef = useRef<HTMLSpanElement | null>(null);
  const scrubRef = useRef<HTMLInputElement | null>(null);
  const mjdChipRef = useRef<HTMLSpanElement | null>(null);
  const detChipRef = useRef<HTMLSpanElement | null>(null);
  const flagChipRef = useRef<HTMLSpanElement | null>(null);
  const blinkChipRef = useRef<HTMLSpanElement | null>(null);

  const clampIndex = useCallback((i: number) => clampIdx(i, n), [n]);
  const frameIndex = clampIndex(transport.frameIndex);
  const anchor = clampIndex(blinkA);
  const displayIndex =
    transport.blink && blinkPhase === 0 ? anchor : frameIndex;
  const current = frames[displayIndex] ?? null;
  const reduceMotion = useMediaQuery("(prefers-reduced-motion: reduce)");

  const intervalCache = useRef(new Map<string, Interval>());

  // Live playback state: index/phase keep their values across renders
  // while everything else tracks the current props.
  const liveRef = useRef<LiveState | null>(null);
  const prevLive = liveRef.current;
  liveRef.current = {
    playing: transport.playing,
    index: prevLive === null ? frameIndex : prevLive.index,
    phase: prevLive === null ? blinkPhase : prevLive.phase,
    frames,
    width: props.width,
    height: props.height,
    stretch: props.stretch,
    cmap: props.cmap,
    signed,
    blink: transport.blink,
    anchor,
    loop: transport.loop,
    reduceMotion,
  };

  useEffect(() => {
    const canvas = canvasRef.current;
    if (canvas === null) return;
    const r = createFrameRenderer(canvas);
    rendererRef.current = r;
    return () => {
      r.dispose();
      rendererRef.current = null;
    };
  }, []);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (canvas !== null) {
      canvas.width = props.width;
      canvas.height = props.height;
    }
  }, [props.width, props.height]);

  const intervalFor = useCallback(
    (f: DecodedFrame): Interval => {
      const key = `${f.meta.id}:${props.stretch}:${props.cmap}:${signed}`;
      const hit = intervalCache.current.get(key);
      if (hit !== undefined) return hit;
      const iv = frameInterval(f.pixels, props.stretch, {
        diverging: isDivergingCmap(props.cmap),
        signed,
      });
      intervalCache.current.set(key, iv);
      return iv;
    },
    [props.stretch, props.cmap, signed],
  );

  // Imperative paint: upload + draw the frame and rewrite the live DOM.
  // Used by the playback loop and by scrub/step while playing. `index` is
  // the series position; `shown` resolves to the blink anchor on phase A.
  const paintLive = useCallback(
    (index: number, phase: 0 | 1) => {
      const st = liveRef.current;
      if (st === null) return;
      const total = st.frames.length;
      const i = clampIdx(index, total);
      const shown = st.blink && phase === 0 ? clampIdx(st.anchor, total) : i;
      st.index = i;
      st.phase = phase;
      const f = st.frames[shown];
      const r = rendererRef.current;
      if (r !== null && f !== undefined) {
        if (lastPixelsRef.current !== f.pixels) {
          r.upload(f.pixels, st.width, st.height);
          lastPixelsRef.current = f.pixels;
        }
        const key = `${f.meta.id}:${st.stretch}:${st.cmap}:${st.signed}`;
        let iv = intervalCache.current.get(key);
        if (iv === undefined) {
          iv = frameInterval(f.pixels, st.stretch, {
            diverging: isDivergingCmap(st.cmap),
            signed: st.signed,
          });
          intervalCache.current.set(key, iv);
        }
        r.draw({ lo: iv.lo, hi: iv.hi, stretch: st.stretch, cmap: st.cmap });
      }
      syncLiveDom(
        {
          canvas: canvasRef.current,
          sr: srRef.current,
          count: countRef.current,
          scrub: scrubRef.current,
          mjd: mjdChipRef.current,
          det: detChipRef.current,
          flag: flagChipRef.current,
          blink: blinkChipRef.current,
        },
        f?.meta,
        shown,
        i,
        total,
        phase,
      );
    },
    [],
  );

  useEffect(() => {
    const r = rendererRef.current;
    if (r === null || current === null || transport.playing) return;
    if (lastPixelsRef.current !== current.pixels) {
      r.upload(current.pixels, props.width, props.height);
      lastPixelsRef.current = current.pixels;
    }
    const iv = intervalFor(current);
    r.draw({ lo: iv.lo, hi: iv.hi, stretch: props.stretch, cmap: props.cmap });
  }, [
    current,
    intervalFor,
    props.stretch,
    props.cmap,
    props.width,
    props.height,
    transport.playing,
  ]);

  // requestAnimationFrame playback clock. The loop reads the live index,
  // frames, loop flag and blink state through liveRef so it is not torn
  // down and restarted on every step, and it paints imperatively instead
  // of going through React/URL state (the old path fired one
  // history.replaceState per played frame).
  const lastBlinkSwapRef = useRef(0);
  const hasFrames = n > 0;
  useEffect(() => {
    if (!transport.playing || !hasFrames) return;
    let raf = 0;
    let last = performance.now();
    let acc = 0;
    const tick = (now: number) => {
      const clock = advancePlayClock(acc, now - last, transport.fps);
      acc = clock.acc;
      last = now;
      if (clock.steps > 0) {
        const st = liveRef.current;
        if (st !== null) {
          if (st.blink) {
            // The blink comparator swaps two frames in place. Under
            // prefers-reduced-motion the swap rate is capped near 2 Hz
            // (the WCAG flash threshold is ~3/s).
            const minMs = st.reduceMotion ? 500 : 0;
            if (now - lastBlinkSwapRef.current >= minMs) {
              lastBlinkSwapRef.current = now;
              paintLive(st.index, st.phase === 0 ? 1 : 0);
            }
          } else if (st.index >= st.frames.length - 1) {
            if (st.loop) {
              paintLive(0, st.phase);
            } else {
              onTransport({ playing: false, frameIndex: st.index });
            }
          } else {
            paintLive(st.index + 1, st.phase);
          }
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [transport.playing, transport.fps, hasFrames, paintLive, onTransport]);

  // Seed the live position when playback starts; reset the blink phase
  // when it stops (the paused view always anchors on A). frameIndex is
  // read through a ref: while playing the committed index is stale by
  // design, so it must not retrigger the effect.
  const frameIndexRef = useRef(frameIndex);
  frameIndexRef.current = frameIndex;
  useEffect(() => {
    if (transport.playing) {
      const st = liveRef.current;
      if (st !== null) {
        st.index = frameIndexRef.current;
        st.phase = 0;
      }
      lastBlinkSwapRef.current = 0;
    } else {
      setBlinkPhase(0);
    }
  }, [transport.playing]);

  // Play/pause commits the live index into the URL exactly once — that is
  // the only history write a playback run needs.
  const setPlaying = (playing: boolean) => {
    const st = liveRef.current;
    if (!playing && st !== null && st.playing) {
      onTransport({ playing: false, frameIndex: st.index });
    } else {
      onTransport({ playing });
    }
  };

  const step = useCallback(
    (delta: number) => {
      const st = liveRef.current;
      if (st !== null && st.playing) {
        paintLive(st.index + delta, st.phase);
      } else {
        onTransport({ frameIndex: clampIndex(frameIndex + delta) });
      }
    },
    [clampIndex, frameIndex, onTransport, paintLive],
  );

  const seekTo = (i: number) => {
    const st = liveRef.current;
    if (st !== null && st.playing) {
      paintLive(i, st.phase);
    } else {
      onTransport({ frameIndex: i });
    }
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (e.target instanceof HTMLInputElement) return;
    switch (e.key) {
      case " ":
      case "k":
        e.preventDefault();
        setPlaying(!transport.playing);
        break;
      case "ArrowLeft":
        e.preventDefault();
        step(-1);
        break;
      case "ArrowRight":
        e.preventDefault();
        step(1);
        break;
      case "Home":
        e.preventDefault();
        seekTo(0);
        break;
      case "End":
        e.preventDefault();
        seekTo(n - 1);
        break;
      case "l":
        onTransport({ loop: !transport.loop });
        break;
      case "b":
        if (!embed) onTransport({ blink: !transport.blink });
        break;
    }
  };

  const meta = current?.meta;
  const ariaFrame = frameAriaText(meta, displayIndex, n);

  return (
    <div
      className="tl-viewer"
      role="application"
      aria-label="Time-lapse viewer. Arrow keys step frames, space toggles playback."
      tabIndex={0}
      onKeyDown={onKeyDown}
    >
      <div className="tl-stage">
        {current === null ? (
          <div className="tl-empty" role="status">
            No frames loaded
          </div>
        ) : (
          <>
            <canvas
              ref={canvasRef}
              className="tl-canvas"
              role="img"
              aria-label={ariaFrame}
            />
            <div className="tl-overlay" aria-hidden="true">
              <span className="tl-chip num" ref={mjdChipRef}>
                {mjdToIso(meta?.obs_mjd_start ?? NaN)}
              </span>
              <span className="tl-chip num" ref={detChipRef}>
                D{meta?.detector ?? "?"}
                {meta !== undefined
                  ? ` · ${meta.wavelength_um.toFixed(2)} µm`
                  : ""}
              </span>
              {transport.blink ? (
                <span className="tl-chip tl-chip-blink" ref={blinkChipRef}>
                  {blinkPhase === 0 ? "A" : "B"}
                </span>
              ) : null}
              <span
                className="tl-chip tl-flag"
                ref={flagChipRef}
                hidden={meta === undefined || (meta.flags?.length ?? 0) === 0}
              >
                {meta?.flags?.join(", ") ?? ""}
              </span>
            </div>
          </>
        )}
      </div>
      <div className="tl-transport">
        <div className="tl-play">
          <button
            type="button"
            className="tl-btn"
            aria-label="Previous frame"
            title="Previous frame (Left arrow)"
            onClick={() => step(-1)}
            disabled={n === 0}
          >
            <Icon d="M10 4L6 8l4 4" />
          </button>
          <button
            type="button"
            className="tl-btn tl-btn-play"
            aria-label={transport.playing ? "Pause" : "Play"}
            aria-pressed={transport.playing}
            title={transport.playing ? "Pause (Space)" : "Play (Space)"}
            onClick={() => setPlaying(!transport.playing)}
            disabled={n === 0}
          >
            {transport.playing ? (
              <Icon d="M6 4v8M10 4v8" />
            ) : (
              <Icon d="M5.5 3.5v9l7-4.5z" fill />
            )}
          </button>
          <button
            type="button"
            className="tl-btn"
            aria-label="Next frame"
            title="Next frame (Right arrow)"
            onClick={() => step(1)}
            disabled={n === 0}
          >
            <Icon d="M6 4l4 4-4 4" />
          </button>
        </div>
        <input
          type="range"
          className="tl-scrubber"
          aria-label="Frame scrubber"
          ref={scrubRef}
          min={0}
          max={Math.max(n - 1, 0)}
          step={1}
          value={frameIndex}
          onChange={(e) => seekTo(Number(e.currentTarget.value))}
          disabled={n === 0}
        />
        <output className="tl-count" aria-hidden="true">
          <span ref={countRef}>{displayIndex + 1}</span>
          <span className="tl-count-of">/{n}</span>
        </output>
        {!embed ? (
          <div className="tl-aux">
            <button
              type="button"
              className="tl-btn"
              aria-label="Toggle loop"
              aria-pressed={transport.loop}
              title="Loop (L)"
              onClick={() => onTransport({ loop: !transport.loop })}
            >
              <Icon d="M3 7V6a2 2 0 0 1 2-2h7l-2-2M13 9v1a2 2 0 0 1-2 2H4l2 2" />
            </button>
            <button
              type="button"
              className="tl-btn tl-btn-text"
              aria-label="A/B: toggle blink comparator"
              aria-pressed={transport.blink}
              title="Blink A/B (B)"
              onClick={() => onTransport({ blink: !transport.blink })}
            >
              A/B
            </button>
            {transport.blink ? (
              <button
                type="button"
                className="tl-btn tl-btn-text num"
                aria-label={`A=${anchor + 1}: set blink reference to current frame`}
                title="Anchor frame A to the current frame"
                onClick={() => setBlinkA(frameIndex)}
              >
                A={anchor + 1}
              </button>
            ) : null}
            <label className="tl-fps">
              <span className="tl-fps-label num">{transport.fps} fps</span>
              <input
                type="range"
                aria-label="Playback speed"
                min={FPS_MIN}
                max={FPS_MAX}
                step={1}
                value={transport.fps}
                onChange={(e) =>
                  onTransport({ fps: Number(e.currentTarget.value) })
                }
              />
            </label>
          </div>
        ) : null}
      </div>
      <span className="tl-sr" role="status" aria-live="polite" ref={srRef}>
        {ariaFrame}
      </span>
    </div>
  );
}
