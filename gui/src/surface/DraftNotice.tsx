import type { Engine } from "../engine";
import { contextStatus, type Environments } from "../context";
import { MonoLine } from "./MonoLine";

export function DraftNotice({ engine, scope, environments, onReview, onTrouble }: {
  engine: Engine; scope: string; environments?: Environments; onReview: () => void; onTrouble: (message: string) => void;
}) {
  const captured = engine.compositionInfo(scope);
  const status = contextStatus(environments, engine.environmentContext(), captured);
  if (!captured || !(status.changed || status.sessionChanged || status.stale || status.missing)) return null;
  return <div className="surface-draft-notice" role="status">
    <MonoLine segments={[{ text: `draft · ${status.label} · ${status.sessionChanged ? "earlier workspace session" : "original environment context retained"}`, role: "mono-warn" }]} />
    <button type="button" className="cell-action" onClick={() => {
      try { engine.rebaseComposition(scope); onReview(); }
      catch (error) { onTrouble(error instanceof Error ? error.message : String(error)); }
    }}>use selected context for this draft</button>
  </div>;
}
