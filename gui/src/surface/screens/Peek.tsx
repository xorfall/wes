import { failureText } from "../../failure-text";
/**
 * One piece of a result in a plain window: the type's structure, the value, the source, or the failure.
 *
 * ⌘click on a cell's verdict, blocks or source lands here — apart from the tabbed `/open`, because
 * the person asked for one thing and this shows one thing: the piece, and a `copy` chip that puts
 * the same text on the clipboard. `esc` leaves, like every screen.
 */
import { useEffect, useState } from "react";
import type { ErrorRecord, StoredValue } from "../../protocol";
import { staleMessage, type WorkspaceNode } from "../../workspace";
import { ValueView } from "../../views/Result";
import { formatCommand } from "../calc-format";
import { DescribeFailureReading, describeFailureText, describeReportId, useDescribeFailureReport, type DescribeFailureReport } from "../DescribeFailureDetails";
import { MonoLine, type Segment } from "../MonoLine";
import { openJson } from "../open-model";
import type { PeekWhat } from "../peek";
import { leaving, Screen } from "../Screen";
import { SourceView } from "../SourceView";
import { typeStructure } from "../type-structure";
import { presentationType } from "../../presentation/prepare";
import { useStoredGate, WITHDRAWN_TITLE, type StoredIdentity } from "../render/dataset-source";

export interface PeekProps {
  readonly engine?: import("../../engine").Engine;
  readonly staleReason?: string;
  readonly top: readonly Segment[];
  /** What was opened: its name. */
  readonly subject: readonly Segment[];
  readonly what: PeekWhat;
  readonly value?: StoredValue;
  /** The stored result `value` was read from in this session; absent for live samples. */
  readonly stored?: StoredIdentity;
  /** The engine withdrew access to the node's result; nothing of its value is shown or copied. */
  readonly accessWithdrawn?: boolean;
  /** The result's workspace name, which management commands use to refer to it. */
  readonly name?: string;
  /** The command as the engine recorded it. */
  readonly source?: string;
  /** The failure as the engine reported it, whole: code, message and source spans. */
  readonly failure?: string;
  /** The failure's structured record, when the engine gave one: it may name a saved report. */
  readonly failureRecord?: ErrorRecord;
  readonly readStatus?: import("react").ReactNode;
  readonly onClose?: () => void;
  readonly chrome?: "full" | "pane";
}

/** What a peek is made of, from the node and its held value: the one place that decides it. */
export type PeekMaterial = Pick<PeekProps, "value" | "stored" | "accessWithdrawn" | "name" | "source" | "failure" | "failureRecord" | "staleReason">;

/**
 * The material every peek of a node draws from — its value, its command, its failure as the
 * engine reported it, and the failure's record — the same whether the peek is a screen over the
 * session or a window of its own. Both callers take it from here so that neither can leave a
 * piece out.
 */
export function peekOf(node: WorkspaceNode | undefined, value: StoredValue | undefined, stored?: StoredIdentity): PeekMaterial {
  return {
    ...(node?.state === "stale" ? { staleReason: staleMessage(node) } : {}),
    ...(value ? { value } : {}),
    ...(value && stored ? { stored } : {}),
    ...(node?.accessWithdrawn ? { accessWithdrawn: true } : {}),
    ...(value && node?.name ? { name: node.name } : {}),
    ...(node?.command !== undefined ? { source: node.command } : {}),
    ...(node?.failure ? { failure: node.failure } : {}),
    ...(node?.failureRecord ? { failureRecord: node.failureRecord } : {}),
  };
}

/**
 * The text the chip copies: the same thing the screen shows, as text. A failure with a saved
 * report copies the report too, once it has been read.
 */
export function peekText(what: PeekWhat, value?: StoredValue, source?: string, failure?: string, report?: DescribeFailureReport, error?: ErrorRecord): string {
  switch (what) {
    case "type": return value ? typeStructure(presentationType(value)) : "";
    case "value": return openJson(value);
    case "source": return formatCommand(source ?? "");
    case "error": return [failureText(failure, error), ...(report ? [describeFailureText(report)] : [])].filter(part => part !== "").join("\n\n");
  }
}

const COPIED_FOR_MS = 1500;

export function PeekScreen({ engine, top, subject, what, value: read, stored, accessWithdrawn, name, source, failure, failureRecord, staleReason, readStatus, onClose, chrome = "full" }: PeekProps) {
  /* A saved describe report is read the moment the failure is peeked at: the window is for reading it. */
  const reportId = what === "error" ? describeReportId(failureRecord) : undefined;
  const report = useDescribeFailureReport(reportId, reportId !== undefined);
  // A withdrawn stored result leaves neither its type nor its value here, on screen or in the copy
  // text; the command and failure are the engine's own record and stay.
  const { value, withdrawn } = useStoredGate(read, stored, accessWithdrawn ? { accessWithdrawn: true } : undefined);
  const hidden = withdrawn && (what === "type" || what === "value");
  const content = hidden ? WITHDRAWN_TITLE : peekText(what, value, source, failure, report.report, failureRecord);
  const errorText = failureText(failure, failureRecord) || (staleReason ? "" : "This result did not fail.");
  const text = what !== "source" && staleReason ? [`Stale: ${staleReason}`, content].filter(Boolean).join("\n\n") : content;
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), COPIED_FOR_MS);
    return () => clearTimeout(timer);
  }, [copied]);
  const copy = () => {
    void navigator.clipboard?.writeText(text).then(() => setCopied(true)).catch(() => undefined);
  };
  const keys: Segment[] = [{ text: "c", role: "mono-ref" }, { text: " copy", role: "mono-dim" }];
  return (
    <Screen
      name="/peek"
      top={top}
      chrome={chrome}
      subject={[...subject, { text: " · ", role: "mono-faint" }, { text: what, role: "mono-dim" }]}
      onClose={onClose}
      footer={leaving()}
      tools={
        <>
          <button type="button" className="screen-chip cell-action" aria-label="Copy to the clipboard" onClick={copy}>{copied ? "✓ copied" : "⧉ copy"}</button>
          <MonoLine segments={keys} className="open-tab-keys" />
        </>
      }
    >
      <div
        className="open-result surface-terminal"
        aria-label={what}
        tabIndex={0}
        onKeyDown={(event) => {
          if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey || event.target !== event.currentTarget) return;
          if (event.key === "c") { event.preventDefault(); copy(); }
        }}
      >
        {what !== "source" && staleReason && <p className="mono-warn" role="status">Stale: {staleReason}</p>}
        {what !== "source" && !staleReason && !hidden && readStatus}
        {hidden && <p className="mono-warn" role="status">{WITHDRAWN_TITLE}</p>}
        {what === "type" && value && <pre className="inspection-text peek-text" data-single-line={!/[\r\n]/.test(content)} tabIndex={0}>{content}</pre>}
        {what === "error" && <pre className="inspection-text peek-text" data-single-line={!/[\r\n]/.test(errorText)} tabIndex={0}>{errorText}</pre>}
        {what === "error" && reportId && <div className="peek-describe-failure"><DescribeFailureReading read={report} mode="peek" /></div>}
        {what === "value" && value && <ValueView engine={engine} value={value} {...(stored ? { stored } : {})} {...(name ? { name } : {})} />}
        {what === "source" && <SourceView source={text} head={false} />}
      </div>
    </Screen>
  );
}
