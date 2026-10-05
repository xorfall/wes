/**
 * The command as the engine recorded it, read only.
 *
 * A view of a result rather than a draft of one: the stored text is shown, never rewritten, and
 * there is nothing here to submit. `onClose` is what a container that can be dismissed passes; a
 * tab that is simply one of several passes none, and then the head says what this is and no more.
 */
import { useEffect, useRef, useState } from "react";
import { language } from "./language";

export function SourceView({ source, onClose, head = true }: { readonly source: string; readonly onClose?: () => void; /** The `source · read only` line; a screen that already says so passes false. */ readonly head?: boolean }) {
  const parent = useRef<HTMLDivElement>(null);
  const fallback = useRef<HTMLTextAreaElement>(null);
  const [ready, setReady] = useState(false);

  useEffect(() => {
    if (!parent.current) return;
    let dead = false;
    let view: { destroy: () => void } | undefined;
    setReady(false);
    void Promise.all([import("./calc-editor"), language()]).then(([{ makeSourceViewer }, pack]) => {
      if (dead || !parent.current) return;
      const focused = fallback.current !== null && document.activeElement === fallback.current;
      const made = makeSourceViewer(parent.current, source, pack);
      view = made;
      if (focused) made.focus();
      setReady(true);
    }).catch(() => {
      // The complete source remains readable/copyable if the optional editor chunk cannot load.
    });
    return () => { dead = true; view?.destroy(); };
  }, [source]);

  return (
    <div className="cell-source-view surface-sunk" data-single-line={!/[\r\n]/.test(source)} role="group" aria-label="Read-only command source"
      onKeyDown={event => {
        // A view stops only the keys it consumes: its own close, and the submit/repeat chords that
        // would otherwise act on a draft. Every other key — Escape included, when there is no
        // `onClose` here — belongs to the container, whose screen closes on it.
        if (event.key === "Escape" && onClose) { event.preventDefault(); event.stopPropagation(); onClose(); return; }
        if ((event.metaKey || event.ctrlKey) && (event.key === "Enter" || event.key.toLowerCase() === "r")) { event.preventDefault(); event.stopPropagation(); }
      }}>
      {head && <div className="cell-source-head">
        <span className="mono-faint">source · read only</span>
        {onClose && <button type="button" className="cell-action" onClick={onClose}>close source</button>}
      </div>}
      {!ready && <textarea ref={fallback} className="cell-source-fallback" value={source} readOnly
        aria-label="Command source" wrap="off" spellCheck={false} autoFocus
        rows={Math.min(12, source.split(/\r\n|\r|\n/).length)} />}
      <div ref={parent} className="cell-source-code" />
    </div>
  );
}
