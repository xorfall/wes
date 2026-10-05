import { useState } from "react";
import type { Engine } from "../engine";
import type { Language } from "./language";
import type { Segment } from "./MonoLine";
import { EditScreen } from "./screens/Edit";
/** Each editor pane owns its draft and composition context. Opening another cannot overwrite it. */
export function PaneEditor({ initial, engine, scope, language, names, top, context, focused, onRun, onRepeat, onClose }: {
  initial: string; engine: Engine; scope: string; language: Language; names: readonly string[];
  top: readonly Segment[]; context: readonly Segment[]; focused: boolean;
  onRun: (source: string, scope: string) => string | undefined;
  onRepeat: (cell: string) => void; onClose: () => void;
}) {
  const [source, setSource] = useState(initial);
  const [cell, setCell] = useState<string>();
  return <EditScreen chrome="pane" autoFocus={focused} source={source} language={language} names={names} top={top} context={context}
    onChange={text => { engine.compose(text, scope); setSource(text); }}
    onRun={text => { engine.compose(text, scope); setCell(onRun(text, scope)); }}
    onRunAgain={() => { if (cell) onRepeat(cell); }} onClose={onClose} />;
}
