import { useState, useSyncExternalStore } from "react";
import { createInteractiveState, type InteractiveState } from "./interactive-state";
import type { Engine } from "../engine";
import { InteractivePreview, type InteractiveModel } from "./forms/Interactive";
import { MonoLine } from "./MonoLine";

/** Mounted with generation/node/run as its key; answers never migrate to a new conversation. */
export function InteractiveResponse({ engine, node, run, model, state }: {
  engine: Engine; node: string; run: string; model: InteractiveModel; state?: InteractiveState;
}) {
  const [local] = useState(createInteractiveState);
  const current = state ?? local;
  const { answer, busy, problem } = useSyncExternalStore(current.subscribe, current.getSnapshot);
  const setAnswer = (answer: string) => current.update({ answer });
  const send = async (eof: boolean) => {
    if (current.getSnapshot().busy) return;
    current.update({ busy: true, problem: undefined });
    // A lost acknowledgement may already have delivered input. Never retain an automatic resend.
    const text = answer;
    setAnswer("");
    try {
      if (eof) await engine.eof(node, run);
      else await engine.answer(node, run, `${text}\n`);
    } catch (error) {
      current.update({ problem: `${error instanceof Error ? error.message : String(error)} · input delivery is unconfirmed; inspect the process before sending again` });
    } finally {
      current.update({ busy: false });
    }
  };
  return <div className="interactive-response">
    <InteractivePreview model={model} answer={answer} onAnswer={setAnswer} onSend={() => void send(false)} disabled={busy} />
    <button type="button" className="cell-action" disabled={busy} onClick={() => void send(true)}>close input (EOF)</button>
    {problem && <MonoLine className="mono-warn" segments={[{ text: problem, role: "mono-warn" }]} />}
  </div>;
}
