// Lazy loader + typed constants for Aladin Lite v3, served by CDS at a pinned
// version URL: https://aladin.cds.unistra.fr/AladinLite/api/v3/<v>/aladin.js
// (docs: "prefer working with a fixed version"; the CDN directory drops the
// git tag's "v" prefix). Aladin Lite is LGPL-3.0, © CDS, Université de
// Strasbourg. Catalog definitions are data, not code: VizieR IDs below.

export const ALADIN_VERSION = "3.8.2";
export const ALADIN_URL = `https://aladin.cds.unistra.fr/AladinLite/api/v3/${ALADIN_VERSION}/aladin.js`;
// Same pinned version from npm via jsDelivr (an ES module whose default
// export is the `A` namespace). Used when the CDS URL fails: it answered
// 404 for every api/v3 version on 2026-11-15.
export const ALADIN_FALLBACK_URL = `https://cdn.jsdelivr.net/npm/aladin-lite@${ALADIN_VERSION}/dist/aladin.js`;

export type CatalogKey = "gaia" | "allwise";

export interface CatalogDef {
  key: CatalogKey;
  label: string;
  vizierId: string;
  color: string;
  attribution: string;
}

// Gaia DR3 = VizieR I/355/gaiadr3 (ESA/Gaia DPAC); AllWISE = VizieR
// II/328/allwise (NASA/IPAC). Both are queried through CDS VizieR.
export const CATALOGS: readonly CatalogDef[] = [
  {
    key: "gaia",
    label: "Gaia DR3",
    vizierId: "I/355/gaiadr3",
    color: "#66d9ef",
    attribution: "Gaia DR3 (ESA, VizieR I/355)",
  },
  {
    key: "allwise",
    label: "AllWISE",
    vizierId: "II/328/allwise",
    color: "#ffb454",
    attribution: "AllWISE (NASA/IPAC, VizieR II/328)",
  },
];

export const ALADIN_ATTRIBUTION = `Aladin Lite v${ALADIN_VERSION} © CDS (LGPL-3.0)`;

export interface AladinCatalogOptions {
  name?: string;
  color?: string;
  shape?: string;
  sourceSize?: number;
  onClick?: string;
  limit?: number;
}

export interface AladinCatalog {
  hide?: () => void;
  show?: () => void;
  isShowing?: () => boolean;
}

export interface AladinInstance {
  addCatalog(cat: AladinCatalog): void;
  removeLayers(): void;
  gotoRaDec(ra: number, dec: number): void;
  setFov(fovDeg: number): void;
  getRaDec(): [number, number];
}

export interface AladinGlobal {
  init: Promise<unknown>;
  aladin(el: HTMLElement, opts?: Record<string, unknown>): AladinInstance;
  catalogFromVizieR(
    vizierId: string,
    target: string | { ra: number; dec: number },
    radiusDeg: number,
    opts?: AladinCatalogOptions,
    successCallback?: (sources: unknown[]) => void,
  ): AladinCatalog;
}

declare global {
  interface Window {
    A?: AladinGlobal;
  }
}

let pending: Promise<AladinGlobal> | null = null;

function initialized(a: AladinGlobal): Promise<AladinGlobal> {
  return a.init.then(
    () => a,
    () => {
      throw new Error("Aladin Lite failed to initialize");
    },
  );
}

// Classic script from CDS: defines window.A.
function fromScript(): Promise<AladinGlobal> {
  return new Promise<AladinGlobal>((resolve, reject) => {
    const script = document.createElement("script");
    script.src = ALADIN_URL;
    script.async = true;
    script.onload = () => {
      const a = window.A;
      if (a === undefined) {
        reject(new Error("Aladin Lite loaded but window.A is missing"));
        return;
      }
      resolve(a);
    };
    script.onerror = () => {
      script.remove();
      reject(new Error("failed to load Aladin Lite"));
    };
    document.head.appendChild(script);
  }).then(initialized);
}

// ES module from jsDelivr: default export is the A namespace.
function fromModule(): Promise<AladinGlobal> {
  return import(/* @vite-ignore */ ALADIN_FALLBACK_URL).then(
    (mod: { default?: AladinGlobal }) => {
      const a = mod.default;
      if (a === undefined) throw new Error("Aladin Lite module has no default");
      window.A = a;
      return initialized(a);
    },
  );
}

/** Try each source in order; resolve with the first that loads. */
export async function loadAladinFrom(
  sources: readonly (() => Promise<AladinGlobal>)[],
): Promise<AladinGlobal> {
  for (const load of sources) {
    try {
      return await load();
    } catch {
      /* try the next source */
    }
  }
  throw new Error("failed to load Aladin Lite");
}

export function loadAladin(): Promise<AladinGlobal> {
  if (window.A !== undefined) return Promise.resolve(window.A);
  if (pending !== null) return pending;
  pending = loadAladinFrom([fromScript, fromModule]);
  pending.catch(() => {
    pending = null; // allow retry after a failed load
  });
  return pending;
}
