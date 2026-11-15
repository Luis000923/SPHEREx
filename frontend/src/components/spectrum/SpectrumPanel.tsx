// Spectrum panel: flux vs wavelength with 1-sigma error bars and optional
// shaded molecular absorption bands. It is docked by the app shell (under
// the image, or beside it on wide screens) and never floats over other
// controls; the chart fills the panel with a 240px floor. uPlot is loaded
// lazily (see uplot.ts); if it fails, the panel degrades to the textual
// summary (point count, detectors, flags) so data is never lost.
import { useEffect, useMemo, useRef, useState } from "react";
import { formatDecDeg, formatRaDeg } from "../../lib/coords";
import { APERTURE_PX, type AperturePx, type SpectrumResponse } from "./types";
import {
  MOLECULAR_BANDS,
  bandRects,
  detectorsPresent,
  flagCounts,
  layoutBandLabels,
  prepareSpectrum,
  robustYRange,
  type PreparedSpectrum,
  type YRange,
} from "./prepare";
import {
  UPLOT_VERSION,
  loadUPlot,
  type UPlotConstructor,
  type UPlotData,
  type UPlotInstance,
  type UPlotOptions,
} from "./uplot";
import type { SpectrumStatus } from "./useSpectrum";
import { FlagBadges } from "../metadata/FlagBadges";
import { useMediaQuery } from "../layout/useMediaQuery";
import "./spectrum.css";

export interface SpectrumPanelProps {
  open: boolean;
  status: SpectrumStatus;
  spectrum: SpectrumResponse | null;
  error: string | null;
  raDeg: number | null;
  decDeg: number | null;
  aperturePx: AperturePx;
  onApertureChange: (px: AperturePx) => void;
  onClose: () => void;
}

function cssVar(el: HTMLElement, name: string, fallback: string): string {
  const v = getComputedStyle(el).getPropertyValue(name).trim();
  return v === "" ? fallback : v;
}

// The chart fills its host; the host's size comes from the layout (it is
// absolutely sized by spectrum.css), never from the chart itself, so the
// ResizeObserver cannot feed back into a growth loop.
function plotSize(el: HTMLElement): { width: number; height: number } {
  return {
    width: Math.max(Math.floor(el.clientWidth), 160),
    height: Math.max(Math.floor(el.clientHeight), 200),
  };
}

// Tooltip + marker text: three significant digits, kept plain (no units
// rounding) so a 103.8 outlier still reads "103.8".
function fmtNum(v: number): string {
  return Math.abs(v) >= 100 ? v.toFixed(1) : v.toPrecision(3);
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, v));
}

function buildChart(
  UPlot: UPlotConstructor,
  el: HTMLElement,
  spectrum: SpectrumResponse,
  prepared: PreparedSpectrum,
  yRange: YRange | null,
  showBands: boolean,
  tipEl: HTMLElement | null,
): UPlotInstance {
  // Colors and fonts come from tokens.css; the fallbacks only apply when
  // the stylesheet is missing (tests).
  const fg = cssVar(el, "--fg-dim", "gray");
  const grid = cssVar(el, "--border", "gray");
  const accent = cssVar(el, "--accent", "steelblue");
  const bandFill = cssVar(el, "--band-fill", "transparent");
  const bandStroke = cssVar(el, "--band-stroke", "gray");
  const family = cssVar(el, "--font-sans", "sans-serif");

  const xs = prepared.wavelengthUm;
  const ys = prepared.flux;
  const sigmas = prepared.sigma;
  const pointFlags = prepared.flags;
  const pointDets = prepared.detectors;
  const data: UPlotData = [xs, ys];
  const yLo = yRange?.min ?? -Infinity;
  const yHi = yRange?.max ?? Infinity;

  const drawBands = (u: UPlotInstance) => {
    if (!showBands) return;
    const sx = u.scales.x;
    if (sx?.min === undefined || sx.max === undefined) return;
    const { ctx, bbox } = u;
    const rects = bandRects(
      MOLECULAR_BANDS,
      (um) => u.valToPos(um, "x", true),
      sx.min,
      sx.max,
    );
    if (rects.length === 0) return;
    ctx.save();
    ctx.fillStyle = bandFill;
    ctx.strokeStyle = bandStroke;
    ctx.lineWidth = 1;
    ctx.font = `11px ${family}`;
    // Labels stagger onto rows (or hide when even that collides) so
    // adjacent bands like H2O/CH4 never overprint on narrow charts.
    const labels = new Map(
      layoutBandLabels(rects, (t) => ctx.measureText(t).width).map((l) => [
        l.key,
        l,
      ]),
    );
    for (const r of rects) {
      ctx.fillRect(r.x0, bbox.top, r.x1 - r.x0, bbox.height);
      ctx.strokeRect(r.x0 + 0.5, bbox.top, r.x1 - r.x0 - 1, bbox.height);
      const l = labels.get(r.key);
      if (l !== undefined) {
        ctx.save();
        ctx.fillStyle = fg;
        ctx.fillText(l.label, l.x, bbox.top + 14 + l.row * 14);
        ctx.restore();
      }
    }
    ctx.restore();
  };

  const drawErrBars = (u: UPlotInstance) => {
    const { ctx, bbox } = u;
    ctx.save();
    ctx.strokeStyle = accent;
    ctx.lineWidth = 1;
    ctx.globalAlpha = 0.8;
    ctx.beginPath();
    for (let i = 0; i < xs.length; i++) {
      const v = ys[i];
      const s = sigmas[i];
      const x = xs[i];
      if (v === null || v === undefined || s === null || s === undefined || x === undefined) {
        continue;
      }
      const xp = u.valToPos(x, "x", true);
      // Clamp to the plot area: with a robust y-range the bar of an
      // off-scale point must not spill over the axis.
      const yTop = clamp(u.valToPos(v + s, "y", true), bbox.top, bbox.top + bbox.height);
      const yBot = clamp(u.valToPos(v - s, "y", true), bbox.top, bbox.top + bbox.height);
      ctx.moveTo(xp - 3, yTop);
      ctx.lineTo(xp + 3, yTop);
      ctx.moveTo(xp, yTop);
      ctx.lineTo(xp, yBot);
      ctx.moveTo(xp - 3, yBot);
      ctx.lineTo(xp + 3, yBot);
    }
    ctx.stroke();
    ctx.restore();
  };

  // Markers are drawn by hand (uPlot's are uniform per series): a clean
  // point is a small filled dot, a flagged one a hollow ring, and a point
  // outside the robust y-range a triangle at the clipped edge pointing
  // toward it.
  const drawPoints = (u: UPlotInstance) => {
    const { ctx, bbox } = u;
    const top = bbox.top;
    const bottom = bbox.top + bbox.height;
    ctx.save();
    ctx.strokeStyle = accent;
    ctx.fillStyle = accent;
    ctx.lineWidth = 1.4;
    for (let i = 0; i < xs.length; i++) {
      const v = ys[i];
      const x = xs[i];
      if (v === null || v === undefined || x === undefined) continue;
      const xp = u.valToPos(x, "x", true);
      if (xp < bbox.left - 8 || xp > bbox.left + bbox.width + 8) continue;
      const flagged = (pointFlags[i]?.length ?? 0) > 0;
      if (v > yHi || v < yLo) {
        // Off-scale: triangle at the edge, apex toward the point.
        const up = v > yHi;
        const cy = up ? top + 7 : bottom - 7;
        ctx.beginPath();
        ctx.moveTo(xp, up ? cy - 5 : cy + 5);
        ctx.lineTo(xp - 4.5, up ? cy + 3 : cy - 3);
        ctx.lineTo(xp + 4.5, up ? cy + 3 : cy - 3);
        ctx.closePath();
        if (flagged) ctx.stroke();
        else ctx.fill();
        continue;
      }
      const yp = u.valToPos(v, "y", true);
      ctx.beginPath();
      ctx.arc(xp, yp, flagged ? 3.5 : 2.2, 0, Math.PI * 2);
      if (flagged) ctx.stroke();
      else ctx.fill();
    }
    ctx.restore();
  };

  // Hover tooltip: nearest point's value, sigma, detector and flag names.
  const updateTip = (u: UPlotInstance) => {
    if (tipEl === null) return;
    const c = u.cursor;
    const idx = c?.idx;
    if (idx === undefined || idx === null || c?.left === undefined || c.top === undefined) {
      tipEl.hidden = true;
      return;
    }
    const v = ys[idx];
    const s = sigmas[idx];
    const wl = xs[idx];
    const det = pointDets[idx];
    const flags = pointFlags[idx] ?? [];
    const lines: string[] = [];
    const parts: string[] = [];
    if (wl !== undefined) parts.push(`${fmtNum(wl)} µm`);
    parts.push(v === null || v === undefined ? "no data" : `${fmtNum(v)} ${spectrum.unit}`);
    if (v !== null && v !== undefined && s !== null && s !== undefined) {
      parts.push(`± ${fmtNum(s)}`);
    }
    if (det !== undefined) parts.push(`D${det}`);
    lines.push(parts.join("  ·  "));
    if (flags.length > 0) lines.push(`flags: ${flags.join(", ")}`);
    if (v !== null && v !== undefined && (v > yHi || v < yLo)) {
      lines.push("off scale");
    }
    tipEl.textContent = lines.join("\n");
    tipEl.hidden = false;
    // Cursor coords are plot-area pixels; offset by the bbox into the
    // container, then clamp the bubble inside it.
    const px = u.bbox.left + c.left;
    const py = u.bbox.top + c.top;
    const tw = tipEl.offsetWidth;
    const th = tipEl.offsetHeight;
    const cw = el.clientWidth;
    const ch = el.clientHeight;
    let left = px + 12;
    let top = py + 12;
    if (left + tw > cw - 4) left = px - 12 - tw;
    if (left < 4) left = 4;
    if (top + th > ch - 4) top = py - 12 - th;
    if (top < 4) top = 4;
    tipEl.style.left = `${left}px`;
    tipEl.style.top = `${top}px`;
  };

  const font = `12px ${family}`;
  const size = plotSize(el);
  const opts: UPlotOptions = {
    width: size.width,
    height: size.height,
    scales: {
      x: { time: false },
      ...(yRange !== null ? { y: { range: [yRange.min, yRange.max] } } : {}),
    },
    axes: [
      { stroke: fg, grid: { stroke: grid, width: 1 }, ticks: { stroke: grid }, label: "Wavelength (µm)", labelSize: 20, size: 32, font, labelFont: font },
      { stroke: fg, grid: { stroke: grid, width: 1 }, ticks: { stroke: grid }, label: spectrum.unit, labelSize: 20, size: 44, font, labelFont: font },
    ],
    series: [
      {},
      { label: spectrum.unit, stroke: accent, width: 1.5, points: { show: false } },
    ],
    hooks: {
      drawClear: [drawBands],
      draw: [drawErrBars, drawPoints],
      setCursor: [updateTip],
    },
    legend: { show: false },
    cursor: { show: true, points: { size: 8 } },
    padding: [12, 12, 0, 0],
  };
  return new UPlot(opts, data, el);
}

export function SpectrumPanel(props: SpectrumPanelProps) {
  const { open, status, spectrum, error, raDeg, decDeg } = props;
  const [showBands, setShowBands] = useState(true);
  const plotRef = useRef<HTMLDivElement | null>(null);
  const tipRef = useRef<HTMLDivElement | null>(null);
  const rootRef = useRef<HTMLElement | null>(null);
  const chartRef = useRef<UPlotInstance | null>(null);
  const [plotError, setPlotError] = useState<string | null>(null);
  // Chart colors are read from the tokens at build time; rebuild the chart
  // when the color scheme flips so it never shows the other scheme's ink.
  const lightScheme = useMediaQuery("(prefers-color-scheme: light)");

  const prepared = useMemo(
    () => (spectrum === null ? null : prepareSpectrum(spectrum.points)),
    [spectrum],
  );
  // Robust flux-axis range (f11): flagged outliers must not squash it.
  const yRange = useMemo(
    () => (spectrum === null ? null : robustYRange(spectrum.points)),
    [spectrum],
  );
  const dets = useMemo(
    () => (spectrum === null ? [] : detectorsPresent(spectrum.points)),
    [spectrum],
  );
  const flags = useMemo(
    () => (spectrum === null ? [] : flagCounts(spectrum.points).map((f) => f.name)),
    [spectrum],
  );

  // Build/rebuild the chart when data or band visibility changes.
  useEffect(() => {
    const el = plotRef.current;
    if (el === null || !open || spectrum === null || prepared === null) return;
    if (prepared.validCount === 0) return;
    let disposed = false;
    let ro: ResizeObserver | null = null;
    loadUPlot()
      .then((UPlot) => {
        if (disposed || plotRef.current === null) return;
        chartRef.current?.destroy();
        const u = buildChart(
          UPlot,
          plotRef.current,
          spectrum,
          prepared,
          yRange,
          showBands,
          tipRef.current,
        );
        chartRef.current = u;
        setPlotError(null);
        ro = new ResizeObserver(() => {
          const host = plotRef.current;
          if (host !== null) u.setSize(plotSize(host));
        });
        ro.observe(plotRef.current);
      })
      .catch((e: unknown) => {
        if (!disposed) {
          setPlotError(e instanceof Error ? e.message : "chart failed to load");
        }
      });
    return () => {
      disposed = true;
      ro?.disconnect();
      chartRef.current?.destroy();
      chartRef.current = null;
      const tip = tipRef.current;
      if (tip !== null) tip.hidden = true; // no stale tooltip across rebuilds
    };
  }, [open, spectrum, prepared, yRange, showBands, lightScheme]);

  // On small screens the docked panel sits under the viewer; bring it into
  // view when it opens (no-op when it is already visible).
  useEffect(() => {
    const el = rootRef.current;
    if (!open || el === null || typeof el.scrollIntoView !== "function") return;
    el.scrollIntoView({ block: "nearest" });
  }, [open]);

  // Escape closes the sheet.
  const { onClose } = props;
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;

  const posLabel =
    raDeg !== null && decDeg !== null
      ? `${formatRaDeg(raDeg)}  ${formatDecDeg(decDeg)}`
      : "";
  const hasPlot =
    spectrum !== null &&
    status !== "error" &&
    prepared !== null &&
    prepared.validCount > 0;

  return (
    <aside className="sp-panel" aria-label="Spectrum panel" ref={rootRef}>
      <header className="sp-head">
        <h2 className="sp-title">Spectrum</h2>
        {posLabel !== "" ? (
          <span className="sp-pos num" title="Sampled position (RA, Dec)">
            {posLabel}
          </span>
        ) : null}
        <div className="sp-tools">
          <label className="sp-toggle">
            <input
              type="checkbox"
              checked={showBands}
              onChange={(e) => setShowBands(e.currentTarget.checked)}
            />
            <span>Bands</span>
          </label>
          <label
            className="sp-aperture"
            title="Aperture radius in pixels — a larger aperture follows moving stars better"
          >
            <span>Aperture</span>
            <select
              aria-label="Aperture radius in pixels"
              value={props.aperturePx}
              onChange={(e) => {
                const v = Number(e.currentTarget.value);
                if ((APERTURE_PX as readonly number[]).includes(v)) {
                  props.onApertureChange(v as AperturePx);
                }
              }}
            >
              {APERTURE_PX.map((a) => (
                <option key={a} value={a}>
                  {a} px
                </option>
              ))}
            </select>
          </label>
          <button
            type="button"
            className="sp-close"
            aria-label="Close spectrum panel"
            title="Close (Esc)"
            onClick={props.onClose}
          >
            <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
              <path
                d="M4 4l8 8M12 4l-8 8"
                stroke="currentColor"
                strokeWidth="1.5"
                strokeLinecap="round"
              />
            </svg>
          </button>
        </div>
      </header>

      <div className="sp-body">
        {hasPlot ? (
          <div
            ref={plotRef}
            className="sp-plot"
            role="img"
            aria-label={`Spectrum chart, ${prepared.validCount} points`}
          >
            <div ref={tipRef} className="sp-tooltip" hidden />
          </div>
        ) : (
          <div className="sp-placeholder">
            {status === "loading" ? (
              <>
                <div className="sp-skel" aria-hidden="true" />
                <p className="sp-status" role="status">
                  Loading spectrum…
                </p>
              </>
            ) : status === "error" ? (
              <p className="sp-error" role="alert">
                {error ?? "Spectrum request failed."}
              </p>
            ) : spectrum !== null ? (
              <p className="sp-status" role="status">
                No usable spectral points at this position.
              </p>
            ) : (
              <p className="sp-status">Click the image to sample a spectrum.</p>
            )}
          </div>
        )}
        {hasPlot && status === "loading" ? (
          <p className="sp-status sp-updating" role="status">
            Updating…
          </p>
        ) : null}
        {plotError !== null ? (
          <p className="sp-status" role="status">
            Chart unavailable ({plotError}); data summary below.
          </p>
        ) : null}
      </div>

      <footer className="sp-foot">
        {hasPlot ? (
          <p className="sp-meta num">
            {prepared.validCount} points
            {dets.length > 0 ? ` · D${dets.join(" D")}` : ""} · 1σ error bars
          </p>
        ) : null}
        {hasPlot && flags.length > 0 ? (
          <div className="sp-flags">
            <span className="sp-flags-label">Flags</span>
            <FlagBadges flags={flags} />
          </div>
        ) : null}
        <p className="sp-cite">
          Bands: Boogert+2015 · Chart: uPlot {UPLOT_VERSION} (MIT)
        </p>
      </footer>
    </aside>
  );
}
