// Header target-preset menu. Built on a native <details> popover so it is
// fully keyboard operable; picking a target submits its coordinates and
// closes the popup. Targets arrive pre-normalized (see App.tsx). A bouncing
// arrow points at the button on every visit until the user touches it
// (deliberately not persisted: the hint returns on each new page load).
import { useEffect, useRef, useState } from "react";
import type { CoordinateValue } from "../controls/CoordinateSearchBar";

export interface MenuTarget {
  id: string;
  label: string;
  ra: string;
  dec: string;
  fov: number;
  note?: string;
}

export interface TargetMenuProps {
  targets: readonly MenuTarget[];
  onSelect: (v: CoordinateValue) => void;
  /**
   * Warm-up hint fired on hover/focus of a menu item so the backend cache
   * is hot by the time the user clicks. Optional; no-op when absent.
   */
  onPrefetch?: (t: MenuTarget) => void;
}

export function TargetMenu(props: TargetMenuProps) {
  const { targets, onSelect, onPrefetch } = props;
  const ref = useRef<HTMLDetailsElement | null>(null);
  const [touched, setTouched] = useState(false);

  useEffect(() => {
    const close = () => {
      if (ref.current !== null) ref.current.open = false;
    };
    const onPointerDown = (e: PointerEvent) => {
      const el = ref.current;
      if (
        el !== null &&
        el.open &&
        e.target instanceof Node &&
        !el.contains(e.target)
      ) {
        el.open = false;
      }
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, []);

  return (
    <details
      className="tmenu"
      ref={ref}
      onPointerDown={() => setTouched(true)}
      onFocus={() => setTouched(true)}
      onToggle={(e) => {
        if (e.currentTarget.open) setTouched(true);
      }}
    >
      <summary className="tmenu-summary">
        <svg
          className="tmenu-icon"
          viewBox="0 0 16 16"
          aria-hidden="true"
          focusable="false"
        >
          <circle cx="8" cy="8" r="5.5" fill="none" stroke="currentColor" />
          <circle cx="8" cy="8" r="1.6" fill="currentColor" />
        </svg>
        <span>Targets</span>
        {touched ? null : (
          <span className="tmenu-hint" aria-hidden="true">
            <svg viewBox="0 0 24 32" focusable="false">
              <path
                d="M12 30V6M4 13l8-8 8 8"
                fill="none"
                stroke="currentColor"
                strokeWidth="3"
                strokeLinecap="round"
                strokeLinejoin="round"
              />
            </svg>
          </span>
        )}
      </summary>
      <div className="tmenu-list">
        {targets.map((t) => (
          <button
            key={t.id}
            type="button"
            className="tmenu-item"
            title={t.note ?? `${t.ra} ${t.dec}`}
            onMouseEnter={() => onPrefetch?.(t)}
            onFocus={() => onPrefetch?.(t)}
            onClick={() => {
              if (ref.current !== null) ref.current.open = false;
              onSelect({ ra: t.ra, dec: t.dec, fov: t.fov });
            }}
          >
            <span className="tmenu-name">{t.label}</span>
            <span className="tmenu-coords">
              {t.ra} &middot; {t.dec}
            </span>
          </button>
        ))}
      </div>
    </details>
  );
}
