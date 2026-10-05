import { useRef } from "react";
import type { CellView } from "../cells";

const SIZES: readonly CellView[] = ["collapsed", "preview", "expanded"];

export function ResultSize({ value, identity, disabled, onChange }: {
  value: CellView; identity: string; disabled: boolean; onChange: (size: CellView) => void;
}) {
  const buttons = useRef<(HTMLButtonElement | null)[]>([]);
  return <div className="result-size" role="radiogroup" aria-label={`Data size of ${identity}`}>
    {SIZES.map((size, at) => <button key={size} ref={element => { buttons.current[at] = element; }} type="button"
      className="cell-action result-size-choice" role="radio" aria-label={`${size} ${identity}${disabled ? " (unavailable: no stored value)" : ""}`} title={disabled ? `${size} unavailable: no stored value` : size} data-tooltip={disabled ? `${size} unavailable: no stored value` : size}
      aria-checked={size === value} tabIndex={size === value ? 0 : -1} disabled={disabled} onClick={() => onChange(size)}
      onKeyDown={event => {
        const next = event.key === "Home" ? 0 : event.key === "End" ? 2
          : ["ArrowLeft", "ArrowUp"].includes(event.key) ? (at + 2) % 3
          : ["ArrowRight", "ArrowDown"].includes(event.key) ? (at + 1) % 3 : undefined;
        if (next === undefined || disabled) return;
        event.preventDefault(); event.stopPropagation(); onChange(SIZES[next]!);
        buttons.current[next]?.focus({ preventScroll: true });
      }}>
      <svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round">
        {size === "collapsed" ? <path d="M4 11h16v3H4z" /> : size === "preview" ? <><rect x="4" y="5" width="16" height="14" rx="1" /><path d="M4 11h16" /></>
          : <><rect x="4" y="4" width="16" height="16" rx="1" /><path d="M7 8h10M7 12h10M7 16h6" /></>}
      </svg>
    </button>)}
  </div>;
}
