import { valueViewModules } from "../../value-views/registry";
/**
 * `/open` shares one stored value across the result, readable JSON, run metadata — and every view
 * the result admits, which `result-views.tsx` answers for and which appear here as tabs of their
 * own. A registered view needs no line in this file: the strip, the panel and the keys read the
 * registry, so the result tab stays the result and never swaps itself for a special renderer.
 */
import { Component, useState, useSyncExternalStore, type ReactNode } from "react";
import type { StoredValue } from "../../protocol";
import { EncodedData, ReadableJson } from "../ResultInspection";
import { viewsFor, type ResultView, type ViewSubject } from "../result-views";
import { MonoLine, type Segment } from "../MonoLine";
import { couldNotDraw } from "../open-model";
import { evidenceLabel } from "../record-progress";
import { RecordProgress } from "../RecordProgress";
import { ScanReceiptDetails } from "../ScanReceiptDetails";
import { leaving, Screen } from "../Screen";
import { ValueView } from "../../views/Result";

/**
 * `result`, `json`, `details` — and the name of any registered view, which is why this is not a
 * closed union. An address naming a tab nothing answers to falls back to the result.
 */
export type OpenTab = "result" | "json" | "details" | (string & {});
/** The three that exist for every result, whatever it is. Views are inserted between them. */
export const OPEN_TABS: readonly OpenTab[] = ["result", "json", "details"];

/** The strip this result wears: the views it admits sit between its JSON and its details. */
export function tabsFor(views: readonly ResultView[]): readonly OpenTab[] {
  return ["result", "json", ...views.map((view) => view.name), "details"];
}

export interface OpenProps {
  readonly top: readonly Segment[];
  /** What was opened: its name, the command that made it, when, its shape, its retention. */
  readonly subject: readonly Segment[];
  readonly tab: OpenTab;
  readonly onTab?: (tab: OpenTab) => void;
  readonly json?: string;
  readonly value?: StoredValue;
  /** What the views are asked about: the node, the value read back, the engine for a live one. */
  readonly viewing?: ViewSubject;
  readonly details?: readonly (readonly Segment[])[];
  readonly readStatus?: ReactNode;
  /**
   * The shared live presentation of an active stream result. When present it is the result tab:
   * a stream has no stored value to read, only a bounded display window read on demand.
   */
  readonly live?: ReactNode;
  readonly onClose?: () => void;
  /** `pane` when the screen is inside a split rather than over the workspace. */
  readonly chrome?: "full" | "pane";
}

/** `⇥ next tab   v json   d details` */
export function tabKeys(): Segment[] {
  return [
    { text: "⇥", role: "mono-ref" }, { text: " next tab", role: "mono-dim" },
    { text: "   ", role: "mono-faint" },
    { text: "v", role: "mono-ref" }, { text: " json", role: "mono-dim" },
    { text: "   ", role: "mono-faint" },
    { text: "d", role: "mono-ref" }, { text: " details", role: "mono-dim" },
  ];
}

/**
 * One tab that fails is one tab, not the whole client.
 *
 * A value arrives as whatever the engine encoded; the client reads it with a cast and draws it. A
 * shape nobody anticipated therefore throws inside a render, and React answers a throw by unmounting
 * the tree — so the symptom of one odd field used to be the window going away. The other two tabs
 * are usually fine, and even the failing one can still say what went wrong, which is more than an
 * empty window says.
 *
 * Keyed on the tab, so switching away and back tries again rather than staying broken.
 */
class Drawn extends Component<{ readonly tab: OpenTab; readonly children: ReactNode }, { readonly failure?: Error }> {
  override state: { readonly failure?: Error } = {};

  static getDerivedStateFromError(failure: Error): { readonly failure: Error } {
    return { failure };
  }

  override render(): ReactNode {
    const { failure } = this.state;
    if (failure === undefined) return this.props.children;
    return <MonoLine segments={couldNotDraw(this.props.tab, failure.message || String(failure))} />;
  }
}

export function OpenScreen({ top, subject, tab, onTab, json, viewing, value, details, readStatus, live, onClose, chrome = "full" }: OpenProps) {
  useSyncExternalStore(valueViewModules.subscribe, valueViewModules.get, valueViewModules.get);
  const views = viewing ? viewsFor(viewing) : [];
  const tabs = tabsFor(views);
  // An address can name a view this result turned out not to admit; the result is always there.
  const showing = tabs.includes(tab) ? tab : "result";
  const view = views.find((it) => it.name === showing);
  const next = () => onTab?.(tabs[(tabs.indexOf(showing) + 1) % tabs.length]!);
  return (
    <Screen
      name="/open"
      top={top}
      chrome={chrome}
      subject={subject}
      onClose={onClose}
      footer={leaving({ text: "p", role: "mono-ref" }, { text: " open in a pane instead", role: "mono-dim" })}
      tools={
        <>
          {tabs.map((name) => (
            <button
              key={name}
              type="button"
              className={`screen-chip ${name === showing ? "chip-chosen" : "cell-action"}`}
              role="tab"
              aria-selected={name === showing}
              onClick={() => onTab?.(name)}
            >
              {name}
            </button>
          ))}
          <MonoLine segments={tabKeys()} className="open-tab-keys" />
        </>
      }
    >
      <div
        className="open-result surface-terminal"
        role="tabpanel"
        aria-label={showing}
        onKeyDown={(event) => {
          if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey || event.target !== event.currentTarget) return;
          if (event.key === "Tab") { event.preventDefault(); next(); }
          if (event.key === "v") onTab?.("json");
          if (event.key === "d") onTab?.("details");
        }}
        tabIndex={0}
      >
        {evidenceLabel(viewing?.node) && <MonoLine segments={[{ text: evidenceLabel(viewing?.node)!, role: "mono-warn" }]} />}
        {/* The same three rows on every tab; details is where they are drawn whole, with the receipt. */}
        <RecordProgress node={viewing?.node} full={showing === "details"} />
        <Drawn tab={showing} key={showing}>
          {showing !== "details" && readStatus}
          {showing === "result" && (live ?? <OpenResult value={value} engine={viewing?.engine} />)}
          {showing === "json" && (value ? <ReadableJson value={value} /> : <pre className="inspection-text open-json" tabIndex={0}>{json ?? ""}</pre>)}
          {view && viewing && <view.Draw subject={viewing} />}
          {showing === "details" && <ScanReceiptDetails value={value} open />}
          {showing === "details" && (details ?? []).map((line, at) => <MonoLine key={at} segments={line} />)}
          {showing === "details" && value && <EncodedData value={value} />}
        </Drawn>
      </div>
    </Screen>
  );
}

/** The result, as the result: the table it is, and the whole value under a disclosure. */
function OpenResult({ value, engine }: Pick<OpenProps, "value"> & {engine?: import("../../engine").Engine}) {
  const [complete, setComplete] = useState(false);
  return <>
    {value && <ValueView engine={engine} value={value} />}
    {value && <details onToggle={event => setComplete(event.currentTarget.open)}><summary>complete value</summary>{complete && <ReadableJson value={value} />}</details>}
  </>;
}
