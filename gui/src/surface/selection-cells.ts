import { useEffect, type Dispatch, type SetStateAction } from "react";
import type { Engine } from "../engine";
import type { Cell } from "../cells";

/** Both terminal surfaces receive the same typed authoring/execution state updates. */
export function useSelectionCells(engine: Engine, setCells: Dispatch<SetStateAction<readonly Cell[]>>, claim?: (attempt: string) => void) {
  useEffect(() => engine.selections.subscribe(update => {
    setCells(previous => previous.map(cell => {
      if (cell.id !== update.cell || update.error && (cell.lastRun !== update.attempt || (cell.state !== "running" && cell.state !== "waiting"))) return cell;
      claim?.(update.attempt);
      return { ...cell, selectionSource: update.source, state: update.state, lastRun: update.attempt,
        ...(update.origin ? { originAttempt: update.origin, attempts: [...new Set([...(cell.attempts ?? []), cell.lastRun])] } : {}),
        ...(update.error ? { submissionRefusal: update.error } : { submissionRefusal: undefined }), diagnostics: [] };
    }));
  }), [engine, setCells, claim]);
}
