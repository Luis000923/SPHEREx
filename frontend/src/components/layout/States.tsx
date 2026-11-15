// Full-area states for the main viewer region: empty (no query yet or no
// data), loading (single skeleton block, no spinners) and error with a
// Retry action. They fill .app-main the same way the canvas does.
export interface EmptyStateProps {
  title?: string;
  hint?: string;
}

export function EmptyState(props: EmptyStateProps) {
  const {
    title = "Point at the sky",
    hint = "Enter RA/Dec above or pick a target to load a SPHEREx time-lapse.",
  } = props;
  return (
    <div className="state state-empty">
      <div className="state-inner">
        {/* Thin dotted trajectory ending at a small body inside a reticle:
            the brand's "flight path" decoration, drawn as a sky track. */}
        <svg
          className="state-icon"
          viewBox="0 0 120 64"
          aria-hidden="true"
          focusable="false"
        >
          <path
            className="state-trail"
            d="M4 58C30 54 58 44 80 30"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.5"
            strokeLinecap="round"
          />
          <circle
            cx="92"
            cy="24"
            r="14"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.5"
          />
          <path
            d="M92 4v6M92 38v6M72 24h6M106 24h6"
            stroke="currentColor"
            strokeWidth="1.5"
            strokeLinecap="round"
          />
          <circle className="state-body" cx="92" cy="24" r="4" strokeWidth="1" />
        </svg>
        <p className="state-title">{title}</p>
        <p className="state-hint">{hint}</p>
      </div>
    </div>
  );
}

export function LoadingSkeleton() {
  return (
    <div className="state-loading" role="status">
      <div className="skel" aria-hidden="true" />
      <span className="visually-hidden">Loading frames…</span>
    </div>
  );
}

export interface ErrorStateProps {
  message: string;
  onRetry: () => void;
}

export function ErrorState(props: ErrorStateProps) {
  const { message, onRetry } = props;
  return (
    <div className="state state-error" role="alert">
      <div className="state-inner">
        <svg
          className="state-icon"
          viewBox="0 0 48 48"
          aria-hidden="true"
          focusable="false"
        >
          <circle
            cx="24"
            cy="24"
            r="19"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
          />
          <path
            d="M24 14v12"
            stroke="currentColor"
            strokeWidth="2.5"
            strokeLinecap="round"
          />
          <circle cx="24" cy="33" r="1.8" fill="currentColor" />
        </svg>
        <p className="state-title">Could not load frames</p>
        <p className="state-hint">{message}</p>
        <button
          type="button"
          className="btn-primary state-retry"
          onClick={onRetry}
        >
          Retry
        </button>
      </div>
    </div>
  );
}
