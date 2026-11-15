// Preset pills for the five official challenge targets. Positions are the
// SIMBAD J2000 astrometry propagated to epoch 2026.0 (the SPHEREx survey
// epoch) — three of the objects move several arcsec/yr and would sit
// outside their own field at J2000. Same table the backend serves from
// /api/v1/targets (b11-presets-epoch).
import { formatDecDeg, formatRaDeg } from "./coords";

export interface PresetTarget {
  id: string;
  label: string;
  ra: string;
  dec: string;
  fov: number;
  note?: string;
}

export const PRESET_TARGETS: readonly PresetTarget[] = [
  {
    id: "barnards-star",
    label: "Barnard's Star",
    ra: "17h57m47.102s", // 269.4462583
    dec: "+04d46m05.97s", // +4.7683250
    fov: 0.1,
    note: "Epoch 2026.0 position (proper motion applied). Highest proper-motion star known.",
  },
  {
    id: "epsilon-indi-b",
    label: "Epsilon Indi B",
    ra: "22h04m23.201s", // 331.0966708
    dec: "-56d48m02.34s", // -56.8006500
    fov: 0.1,
    note: "Epoch 2026.0 position. Brown-dwarf binary Ba+Bb, unresolved at 6.15\"/px.",
  },
  {
    id: "wise-0855",
    label: "WISE 0855",
    ra: "08h55m02.344s", // 133.7597667
    dec: "-07d14m31.86s", // -7.2421833
    fov: 0.1,
    note: "Epoch 2026.0 position. The challenge catalogue position 08h55m10.831s -07d14m42.53s is the AllWISE epoch 2010.5. A very faint, extremely cold brown dwarf: best seen in the long-wavelength detectors (D5, D6) with Co-add or the ZScale stretch. Its ~8\"/yr motion (~1.3 native pixels) is near the limit of what SPHEREx pixels show.",
  },
  {
    id: "rw-aurigae",
    label: "RW Aurigae",
    ra: "05h07m49.581s", // 76.9565875
    dec: "+30d24m04.65s", // +30.4012917
    fov: 0.05,
    note: "Epoch 2026.0 position. Young variable star.",
  },
  {
    id: "r-draconis",
    label: "R Draconis",
    ra: "16h32m40.136s", // 248.1672333
    dec: "+66d45m18.09s", // +66.7550250
    fov: 0.05,
    note: "Epoch 2026.0 position. Mira variable at high declination.",
  },
];

export interface ServerTarget {
  id: string;
  name: string;
  ra_deg: number;
  dec_deg: number;
  fov_deg: number;
  note?: string | undefined;
}

// Map /api/v1/targets rows to menu entries. Known ids keep the built-in
// sexagesimal strings byte-identical: the backend's baked cache is keyed on
// the exact query string, so a re-formatted position ("47.10s" instead of
// "47.102s") misses it and fails offline. Unknown ids are formatted with
// milli-second / centi-arcsecond precision.
export function menuTargetsFromServer(
  targets: readonly ServerTarget[],
): PresetTarget[] {
  return targets.map((t) => {
    const builtin = PRESET_TARGETS.find((p) => p.id === t.id);
    const entry: PresetTarget = {
      id: t.id,
      label: t.name,
      ra: builtin?.ra ?? formatRaDeg(t.ra_deg, 3),
      dec: builtin?.dec ?? formatDecDeg(t.dec_deg, 2),
      fov: t.fov_deg,
    };
    const note = t.note ?? builtin?.note;
    if (note !== undefined) entry.note = note;
    return entry;
  });
}
