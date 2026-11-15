// Composition root: wires URL state, the f1 viewer/controls, the f2
// spectrum/metadata/overlay components and the API client into the f4 app
// shell. apiBase is injected and defaults to "" so requests hit the
// backend's /api/v1 routes on the same origin (the client in lib/api.ts
// appends /api/v1 itself). ?mock=1 swaps every wire payload for the
// synthetic mocks so the full UI runs with no backend (demos, e2e tests).
import {
  Suspense,
  lazy,
  useCallback,
  useEffect,
  useMemo,
  useState,
} from "react";
import { AppShell } from "./components/layout/AppShell";
import { FirstRunHint } from "./components/about/FirstRunHint";
import { GitHubLink } from "./components/about/GitHubLink";
import { EmbedCredit } from "./components/about/EmbedCredit";
import type { MenuTarget } from "./components/layout/TargetMenu";
import {
  EmptyState,
  ErrorState,
  LoadingSkeleton,
} from "./components/layout/States";
import {
  CoordinateSearchBar,
  type CoordinateValue,
} from "./components/controls/CoordinateSearchBar";
import { ModeSelector } from "./components/controls/ModeSelector";
import {
  TimeLapseCanvas,
  type TransportState,
} from "./components/viewer/TimeLapseCanvas";
import {
  canvasClickToSpectrum,
  useSpectrum,
} from "./components/spectrum/useSpectrum";
import {
  createSpectrumClient,
  type SpectrumClient,
} from "./components/spectrum/spectrumApi";
import { makeMockSpectrum } from "./components/spectrum/mockSpectrum";
import { FrameMetadata } from "./components/metadata/FrameMetadata";
import { useUrlState } from "./hooks/useUrlState";
import {
  MOCK_PARTIAL_MS,
  useFrames,
  type FramesResult,
} from "./hooks/useFrames";
import { fetchTargets, prefetchFrames } from "./lib/api";
import { parseDecToDeg, parseRaToDeg } from "./lib/coords";
import { makeMockFramesResponse } from "./lib/mock";
import {
  PRESET_TARGETS,
  menuTargetsFromServer,
  type PresetTarget,
} from "./lib/presets";
import { isStretchKind, STRETCH_KINDS } from "./lib/stretch";
import { COLORMAP_NAMES, isColormapName } from "./lib/colormaps";
import { MODE_LABELS, type FramesResponse } from "./lib/types";
import type { UrlState } from "./lib/urlState";

const DEFAULT_API_BASE = (import.meta.env.VITE_API_BASE ?? "") as string;

// The Aladin chunk (and the CDN script it loads) only downloads when the
// user enables the overlay.
const AladinOverlay = lazy(() =>
  import("./components/overlays/AladinOverlay").then((m) => ({
    default: m.AladinOverlay,
  })),
);

// The spectrum panel (chart setup + uPlot loader) and the About dialog
// are secondary surfaces: they stay out of the main bundle and download
// on first use.
const SpectrumPanel = lazy(() =>
  import("./components/spectrum/SpectrumPanel").then((m) => ({
    default: m.SpectrumPanel,
  })),
);
const AboutDialog = lazy(() =>
  import("./components/about/AboutDialog").then((m) => ({
    default: m.AboutDialog,
  })),
);

// ?mock=1 renders the full UI from the synthetic payloads (frames +
// spectrum) with no backend — used for demos and Playwright.
function mockModeEnabled(): boolean {
  if (typeof window === "undefined") return false;
  return new URLSearchParams(window.location.search).get("mock") === "1";
}

// ?mock_delay=<ms> stretches the mock "still downloading" phase so the
// pending/partial states can be exercised (e2e). Defaults to a beat.
function mockDelayMs(): number {
  if (typeof window === "undefined") return MOCK_PARTIAL_MS;
  const v = new URLSearchParams(window.location.search).get("mock_delay");
  const n = v === null ? NaN : Number(v);
  return Number.isFinite(n) && n >= 0 ? n : MOCK_PARTIAL_MS;
}

// ?mock_w, ?mock_h and ?mock_n resize the synthetic series so decode and
// render can be exercised at real-payload sizes (192x192, 24 frames).
function mockDims(): { width?: number; height?: number; count?: number } {
  const out: { width?: number; height?: number; count?: number } = {};
  if (typeof window === "undefined") return out;
  const p = new URLSearchParams(window.location.search);
  const dims: Record<string, "width" | "height" | "count"> = {
    mock_w: "width",
    mock_h: "height",
    mock_n: "count",
  };
  for (const [key, field] of Object.entries(dims)) {
    const v = Number(p.get(key));
    if (Number.isFinite(v) && v > 0) out[field] = Math.floor(v);
  }
  return out;
}

// Aperture is a spectrum-request parameter; the mock ignores it because the
// synthetic response does not depend on the aperture size.
function createMockSpectrumClient(): SpectrumClient {
  return {
    spectrum: (raDeg, decDeg) =>
      Promise.resolve(makeMockSpectrum(raDeg, decDeg)),
  };
}

// Disclosure chevron for the collapsible panel sections.
function GroupChevron() {
  return (
    <svg
      className="app-group-chev"
      viewBox="0 0 16 16"
      aria-hidden="true"
      focusable="false"
    >
      <path
        d="M4 6l4 4 4-4"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

// The preset whose position is being viewed, if any: exact string match
// with presets.ts, or within 2 arcsec once parsed (typed variants of the
// same position). Display only; it never changes the query.
export function activePreset(ra: string, dec: string): PresetTarget | null {
  const raDeg = parseRaToDeg(ra);
  const decDeg = parseDecToDeg(dec);
  for (const p of PRESET_TARGETS) {
    if (p.ra === ra && p.dec === dec) return p;
    if (raDeg === null || decDeg === null) continue;
    const pRa = parseRaToDeg(p.ra);
    const pDec = parseDecToDeg(p.dec);
    if (pRa === null || pDec === null) continue;
    let dRa = Math.abs(raDeg - pRa);
    if (dRa > 180) dRa = 360 - dRa;
    const dRaCos = dRa * Math.cos((pDec * Math.PI) / 180);
    const sepArcsec = Math.hypot(dRaCos, decDeg - pDec) * 3600;
    if (sepArcsec <= 2) return p;
  }
  return null;
}

// Pill chip naming the active preset target (all presets are epoch-2026.0
// positions, see lib/presets.ts).
function ActiveTargetChip({ target }: { target: PresetTarget }) {
  return (
    <p className="app-target">
      <span className="chip">
        <span className="visually-hidden">Active target: </span>
        <span className="chip-label">
          {target.label.toUpperCase()} · EPOCH 2026.0
        </span>
      </span>
    </p>
  );
}

export interface AppProps {
  apiBase?: string;
  mockData?: FramesResponse;
}

interface ExplorerProps {
  apiBase: string | null;
  url: UrlState;
  setUrl: (patch: Partial<UrlState>) => void;
  detector: number | null;
  embed: boolean;
  mockData?: FramesResponse;
  mockDelay: number;
  onPick: (raDeg: number, decDeg: number) => void;
  onResult: (r: FramesResult) => void;
  onRetry: () => void;
}

// Owns frame fetching and transport state for the main viewer area. App
// remounts it (key change) on Retry, which re-runs the fetch effect.
function Explorer(props: ExplorerProps) {
  const { url, setUrl, detector, embed, apiBase, mockData, mockDelay } = props;
  const [local, setLocal] = useState({
    playing: false,
    loop: true,
    blink: false,
  });
  const res = useFrames(
    apiBase,
    {
      ra: url.ra,
      dec: url.dec,
      fov: url.fov,
      mode: url.mode,
      detector,
      // max_frames omitted: the contract default (24) is the selection
      // the server pre-warms; 60 fetched 2.5x more cutouts and missed it.
    },
    mockData,
    mockDelay,
  );

  const { onResult } = props;
  useEffect(() => onResult(res), [res, onResult]);

  const transport: TransportState = {
    playing: local.playing,
    frameIndex: url.frame,
    fps: url.fps,
    loop: local.loop,
    blink: local.blink,
  };

  const onTransport = (patch: Partial<TransportState>) => {
    const u: Partial<UrlState> = {};
    if (patch.frameIndex !== undefined) u.frame = patch.frameIndex;
    if (patch.fps !== undefined) u.fps = patch.fps;
    if (
      patch.playing !== undefined ||
      patch.loop !== undefined ||
      patch.blink !== undefined
    ) {
      setLocal((prev) => ({
        playing: patch.playing ?? prev.playing,
        loop: patch.loop ?? prev.loop,
        blink: patch.blink ?? prev.blink,
      }));
    }
    setUrl(u);
  };

  if (res.status === "error") {
    if (res.errorCode === "not_found") {
      return (
        <EmptyState
          title="No SPHEREx observations cover this position yet"
          hint="Try a wider field of view or pick a target."
        />
      );
    }
    return (
      <ErrorState
        message={
          res.errorCode === "upstream_unavailable"
            ? "The NASA archive is not answering right now."
            : (res.error ?? "Request failed.")
        }
        onRetry={props.onRetry}
      />
    );
  }
  if (res.status === "idle") {
    return <EmptyState />;
  }
  const pending = res.pending === true;
  if (res.status === "loading" && res.frames.length === 0) {
    return (
      <>
        <LoadingSkeleton />
        <p className="app-status" role="status">
          {res.errorCode === "upstream_unavailable" && pending
            ? "The NASA archive is slow to answer — retrying…"
            : "Loading frames…"}
        </p>
      </>
    );
  }
  if (res.frames.length === 0) {
    if (pending) {
      return (
        <>
          <LoadingSkeleton />
          <p className="app-status" role="status">
            Loading frames…
          </p>
        </>
      );
    }
    if (url.mode !== "raw") {
      return (
        <EmptyState
          title="Not enough comparable frames at this position"
          hint={`${MODE_LABELS[url.mode]} needs several exposures of the same detector and band. Try Raw or another field.`}
        />
      );
    }
    return (
      <EmptyState
        title="No SPHEREx observations cover this position yet"
        hint="Try a wider field of view or pick a target."
      />
    );
  }

  const width = res.response?.width ?? 64;
  const height = res.response?.height ?? 64;
  const expected = res.expected ?? null;
  const current =
    res.frames[Math.min(Math.max(url.frame, 0), res.frames.length - 1)] ??
    null;

  return (
    <>
      {res.status === "loading" ? (
        <p className="app-status" role="status">
          Updating…
        </p>
      ) : null}
      <div
        className="app-stage"
        onClick={
          embed
            ? undefined
            : canvasClickToSpectrum(
                { frame: current, width, height },
                props.onPick,
              )
        }
      >
        <TimeLapseCanvas
          frames={res.frames}
          width={width}
          height={height}
          unit={res.response?.unit}
          stretch={url.stretch}
          cmap={url.cmap}
          transport={transport}
          onTransport={onTransport}
          embed={embed}
          signed={url.mode === "diff"}
        />
      </div>
      {!embed && pending ? (
        <p className="app-status" role="status">
          {expected !== null
            ? `Loading frames ${res.frames.length} / ${expected}`
            : `Loading more frames… ${res.frames.length} so far`}
        </p>
      ) : !embed && res.settled === true && expected !== null ? (
        <p className="app-status">
          {res.frames.length} of {expected} frames available (the archive
          did not provide the rest).{" "}
          <button
            type="button"
            className="app-status-retry"
            onClick={props.onRetry}
          >
            Check again
          </button>
        </p>
      ) : !embed && res.partial === true && expected !== null ? (
        <p className="app-status">
          Partial: {res.frames.length} of {expected} frames.{" "}
          <button
            type="button"
            className="app-status-retry"
            onClick={props.onRetry}
          >
            Retry
          </button>
        </p>
      ) : !embed &&
        url.mode === "raw" &&
        res.response?.truncated === true ? (
        <p className="app-status">
          Showing {res.frames.length} frames spread over{" "}
          {res.response.total_observations ?? "?"} observations.
        </p>
      ) : null}
    </>
  );
}

export function App(props: AppProps) {
  const { apiBase = DEFAULT_API_BASE } = props;
  const [url, setUrl] = useUrlState();
  const [retryNonce, setRetryNonce] = useState(0);
  const [targets, setTargets] =
    useState<readonly MenuTarget[]>(PRESET_TARGETS);
  const [overlayOn, setOverlayOn] = useState(false);
  const [framesResult, setFramesResult] = useState<FramesResult>({
    status: "idle",
    response: null,
    frames: [],
    error: null,
  });

  // Mock mode: the prop wins, then ?mock=1. Memoized so useFrames sees a
  // stable payload and does not refetch on every render.
  const mockData = useMemo(
    () =>
      props.mockData ??
      (mockModeEnabled() ? makeMockFramesResponse(mockDims()) : undefined),
    [props.mockData],
  );

  const spectrumClient = useMemo<SpectrumClient>(
    () =>
      mockData !== undefined
        ? createMockSpectrumClient()
        : createSpectrumClient(apiBase),
    [apiBase, mockData],
  );
  const spec = useSpectrum(spectrumClient);

  // The preset menu prefers server-provided targets (/api/v1/targets) and
  // silently keeps the built-in presets when the backend is unreachable.
  useEffect(() => {
    if (mockData !== undefined) return;
    const ctrl = new AbortController();
    fetchTargets(apiBase, ctrl.signal)
      .then((ts) => {
        if (ts.length === 0) return;
        setTargets(menuTargetsFromServer(ts));
      })
      .catch(() => {
        /* offline or backend down: keep PRESET_TARGETS */
      });
    return () => ctrl.abort();
  }, [apiBase, mockData]);

  const submitCoords = useCallback(
    (v: CoordinateValue) => setUrl({ ...v, frame: 0 }),
    [setUrl],
  );

  // Hover/focus on a preset warms the backend cache with the same request
  // the click will issue, so the follow-up lands on a hot cache. Skipped
  // in mock mode: there is no backend to warm.
  const prefetchTarget = useCallback(
    (t: MenuTarget) => {
      if (mockData !== undefined) return;
      prefetchFrames(apiBase, {
        ra: t.ra,
        dec: t.dec,
        fov: t.fov,
        mode: url.mode,
        detector: url.det,
      });
    },
    [apiBase, mockData, url.mode, url.det],
  );

  const frames = framesResult.frames;
  const currentFrame =
    frames[Math.min(Math.max(url.frame, 0), Math.max(frames.length - 1, 0))] ??
    null;

  // Overlay view position: the queried position when a response exists,
  // else the coordinates currently entered in the search bar.
  const q = framesResult.response?.query;
  const overlayPos = useMemo(() => {
    const raDeg = q?.ra_deg ?? parseRaToDeg(url.ra);
    const decDeg = q?.dec_deg ?? parseDecToDeg(url.dec);
    if (raDeg === null || raDeg === undefined) return null;
    if (decDeg === null || decDeg === undefined) return null;
    return { raDeg, decDeg, fovDeg: q?.fov_deg ?? url.fov };
  }, [q, url.ra, url.dec, url.fov]);

  const preset = useMemo(
    () => activePreset(url.ra, url.dec),
    [url.ra, url.dec],
  );

  const panel = (
    <div className="app-controls">
      <details className="app-group" open>
        <summary className="app-group-title">
          <h2 id="app-grp-mode">View</h2>
          <GroupChevron />
        </summary>
        <ModeSelector
          mode={url.mode}
          detector={url.det}
          onChange={(p) => {
            if (p.mode !== undefined) setUrl({ mode: p.mode, frame: 0 });
            if (p.detector !== undefined) setUrl({ det: p.detector, frame: 0 });
          }}
        />
      </details>
      <details className="app-group" open>
        <summary className="app-group-title">
          <h2 id="app-grp-display">Display</h2>
          <GroupChevron />
        </summary>
        <div className="app-fields">
          <label className="app-field">
            <span>Stretch</span>
            <select
              value={url.stretch}
              onChange={(e) => {
                const v = e.currentTarget.value;
                if (isStretchKind(v)) setUrl({ stretch: v });
              }}
            >
              {STRETCH_KINDS.map((s) => (
                <option key={s} value={s}>
                  {s}
                </option>
              ))}
            </select>
          </label>
          <label className="app-field">
            <span>Colormap</span>
            <select
              value={url.cmap}
              onChange={(e) => {
                const v = e.currentTarget.value;
                if (isColormapName(v)) setUrl({ cmap: v });
              }}
            >
              {COLORMAP_NAMES.map((c) => (
                <option key={c} value={c}>
                  {c}
                </option>
              ))}
            </select>
          </label>
        </div>
      </details>
      <details className="app-group" open>
        <summary className="app-group-title">
          <h2 id="app-grp-frame">Current frame</h2>
          <GroupChevron />
        </summary>
        <FrameMetadata
          frame={currentFrame?.meta ?? null}
          unit={framesResult.response?.unit}
          flagLegend={framesResult.response?.flag_legend}
        />
      </details>
      <details className="app-group" open>
        <summary className="app-group-title">
          <h2 id="app-grp-overlay">Overlay</h2>
          <GroupChevron />
        </summary>
        <label className="app-check">
          <input
            type="checkbox"
            checked={overlayOn}
            onChange={(e) => setOverlayOn(e.currentTarget.checked)}
          />
          <span>Catalog overlay (Gaia DR3, AllWISE)</span>
        </label>
        {overlayOn ? (
          overlayPos !== null ? (
            <Suspense
              fallback={<p className="app-status">Loading overlay…</p>}
            >
              <AladinOverlay
                raDeg={overlayPos.raDeg}
                decDeg={overlayPos.decDeg}
                fovDeg={overlayPos.fovDeg}
                defaultOpen
              />
            </Suspense>
          ) : (
            <p className="app-status">
              Search for a position to enable the sky overlay.
            </p>
          )
        ) : null}
      </details>
    </div>
  );

  return (
    <AppShell
      embed={url.embed}
      targets={targets}
      onSelectTarget={submitCoords}
      onPrefetchTarget={prefetchTarget}
      headerActions={
        <>
          <GitHubLink />
          <Suspense fallback={null}>
            <AboutDialog />
          </Suspense>
        </>
      }
      notice={<FirstRunHint />}
      credit={<EmbedCredit />}
      search={
        <>
          <CoordinateSearchBar
            value={{ ra: url.ra, dec: url.dec, fov: url.fov }}
            loading={framesResult.status === "loading"}
            onSubmit={submitCoords}
          />
          {preset !== null ? <ActiveTargetChip target={preset} /> : null}
        </>
      }
      panel={panel}
      drawer={
        spec.open ? (
          <Suspense fallback={null}>
            <SpectrumPanel
            open={spec.open}
            status={spec.status}
            spectrum={spec.data}
            error={spec.error}
            raDeg={spec.raDeg}
            decDeg={spec.decDeg}
            aperturePx={spec.aperturePx}
            onApertureChange={spec.setAperture}
            onClose={spec.close}
            />
          </Suspense>
        ) : null
      }
    >
      <Explorer
        key={retryNonce}
        apiBase={apiBase}
        url={url}
        setUrl={setUrl}
        detector={url.det}
        embed={url.embed}
        mockData={mockData}
        mockDelay={mockDelayMs()}
        onPick={spec.requestAt}
        onResult={setFramesResult}
        onRetry={() => setRetryNonce((n) => n + 1)}
      />
    </AppShell>
  );
}
