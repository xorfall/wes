/**
 * A live dataset deletion plan, laid out for review before anything is applied.
 *
 * Everything shown is the plan's own projection: the snapshot it names, the roots that keep it,
 * active readers and writer, and protected bytes. Applying needs two separate, explicit answers —
 * remove the listed references, delete protected data — and neither has a default: until both are
 * answered nothing can be prepared. Even then nothing runs here; the delete command is written into
 * the prompt for the person to submit, and the engine checks the plan's liveness and every root
 * again. Opening or reading a plan never deletes.
 */
import { useContext, useState } from "react";
import type { StoredValue } from "../protocol";
import { groupedDigits } from "../presentation/format";
import { ComposeContext, deleteCommand, deletePlanOf } from "./dataset-management";
import { MonoLine } from "./MonoLine";
import "./dataset-management.css";

type Answer = boolean | undefined;

export function isDeletePlan(value: StoredValue): boolean {
  return value.type.kind === "meta" && value.type.name === "DatasetDeletePlan";
}

export function DeletePlanReview({ value, name, collapsed = false }: { readonly value: StoredValue; readonly name?: string; readonly collapsed?: boolean }) {
  const composer = useContext(ComposeContext);
  const plan = deletePlanOf(value);
  const [references, setReferences] = useState<Answer>();
  const [protectedData, setProtectedData] = useState<Answer>();
  if (!plan) return <MonoLine segments={[{ text: "Deletion plan · this plan's projection could not be read; nothing can be applied from it", role: "mono-warn" }]} className="value-line" />;
  const summary = `Deletion plan · generation ${groupedDigits(plan.generation)} · ${plan.references.length} ${plan.references.length === 1 ? "reference" : "references"} · ${groupedDigits(plan.protectedBytes)} protected bytes`;
  if (collapsed) return <MonoLine segments={[{ text: summary, role: "mono-dim" }]} className="value-line" />;
  const active = plan.activeWriter || plan.activeReaders !== "0";
  const answered = references !== undefined && protectedData !== undefined;
  const why = !name ? "this plan has no result name, so a delete command cannot refer to it"
    : !composer ? "commands can be prepared only in the session"
    : !answered ? "answer both questions to prepare the delete command" : undefined;
  return <section className="delete-plan" aria-label="Deletion plan review">
    <MonoLine segments={[{ text: summary, role: "mono-ink" }]} className="value-line" />
    <MonoLine segments={[{ text: "dataset ", role: "mono-dim" }, { text: plan.dataset, role: "mono-literal" }]} className="value-line" />
    <MonoLine segments={[{ text: "active readers ", role: "mono-dim" }, { text: groupedDigits(plan.activeReaders), role: plan.activeReaders === "0" ? "mono-ink" : "mono-warn" },
      { text: " · writer ", role: "mono-dim" }, { text: plan.activeWriter ? "active" : "none", role: plan.activeWriter ? "mono-warn" : "mono-ink" }]} className="value-line" />
    {active && <MonoLine segments={[{ text: "Stop the active readers and writer explicitly before applying; the plan does not stop them.", role: "mono-warn" }]} className="value-line" />}
    <div className="delete-plan-references" role="list" aria-label="References that keep this dataset">
      {plan.references.length === 0
        ? <MonoLine segments={[{ text: "no references keep this dataset", role: "mono-faint" }]} className="value-line" />
        : plan.references.map((reference, at) => <div role="listitem" key={at}>
          <MonoLine segments={[{ text: reference.kind, role: "mono-param" }, { text: " · ", role: "mono-faint" }, { text: reference.retention, role: "mono-dim" },
            { text: " · ", role: "mono-faint" }, { text: reference.identity, role: "mono-literal" }]} className="value-line" />
        </div>)}
    </div>
    <MonoLine segments={[{ text: plan.notice, role: "mono-faint" }]} className="value-line delete-plan-notice" />
    <fieldset className="delete-plan-choice">
      <legend>Remove the {plan.references.length} listed {plan.references.length === 1 ? "reference" : "references"}?</legend>
      <label><input type="radio" name={`${name ?? "plan"}-references`} checked={references === true} onChange={() => setReferences(true)} /> remove</label>
      <label><input type="radio" name={`${name ?? "plan"}-references`} checked={references === false} onChange={() => setReferences(false)} /> keep them</label>
    </fieldset>
    <fieldset className="delete-plan-choice">
      <legend>Delete {groupedDigits(plan.protectedBytes)} protected bytes?</legend>
      <label><input type="radio" name={`${name ?? "plan"}-protected`} checked={protectedData === true} onChange={() => setProtectedData(true)} /> delete</label>
      <label><input type="radio" name={`${name ?? "plan"}-protected`} checked={protectedData === false} onChange={() => setProtectedData(false)} /> refuse</label>
    </fieldset>
    <div className="delete-plan-actions">
      <button type="button" className="cell-action" disabled={why !== undefined}
        onClick={() => { if (name && composer && answered) composer.compose(deleteCommand(`$${name}`, references, protectedData)); }}>
        prepare delete command
      </button>
      <MonoLine segments={[{ text: why ?? "written into the prompt; nothing runs until you submit it", role: "mono-faint" }]} className="value-line" />
    </div>
  </section>;
}
