/**
 * The special ways one result can be looked at, and which of them a result admits.
 *
 * Registered by name and asked in order, most particular first — `forms/registry.ts` holds the
 * shapes a value takes the same way. The difference is what a match is worth: a form is what a
 * value *is* and there is always one, while a view is an extra way of reading a particular kind of
 * result and most results admit none.
 *
 * Adding a view is adding one entry. The cell's button, the open screen's tab strip and the address
 * `#open/<node>/<view>` all read this list, so none of them names a view of its own and none of
 * them has to be edited to learn about a new one.
 */
import type { ReactNode } from "react";
import type { Engine } from "../engine";
import type { StoredValue } from "../protocol";
import type { WorkspaceNode } from "../workspace";
import { httpTrace, HttpInspection, LiveHttpInspection } from "../views/HttpInspection";
import { ValueView } from "../views/Result";
import { valueViewModules } from "../value-views/registry";
import { SourceView } from "./SourceView";
import {preparedValues} from "./render/ValueBlock";
import { registryStore } from "../presentation/registry-store";

/**
 * What there is to look at: the node the engine reported, the value read back when it has been,
 * and the engine itself for a view that keeps asking — a live trace is still arriving.
 */
export interface ViewSubject {
  readonly value?: StoredValue;
  readonly node?: WorkspaceNode;
  readonly engine?: Engine;
  /** Which run this is, so a view that polls starts again rather than continuing an old one. */
  readonly generation?: string;
  /** The stored result `value` was read from, in this session; absent for live samples. */
  readonly stored?: import("./render/dataset-source").StoredIdentity;
}

export interface ResultView {
  /** The word the address and the button both say: `#open/n7/http` and `⇄ http`. */
  readonly name: string;
  /** Whether this result can be read this way at all, which is whether it gets a tab. */
  readonly matches: (subject: ViewSubject) => boolean;
  readonly Draw: (props: { readonly subject: ViewSubject }) => ReactNode;
}

/** A module's value, rendered by exactly the same host in every surface. */
function moduleViews(): ResultView[] {
  return valueViewModules.get().map(module => ({
    name: valueViewModules.address(module),
    matches: ({ value }) => {
      if(!value)return false;
      const prepared=preparedValues.read("view:discovery",value);
      return valueViewModules.find(prepared.type,prepared.data,
        registryStore.get().match(prepared.type, value.data)?.entry.kind, prepared.viewModules)?.id===module.id;
    },
    Draw: ({ subject }) => subject.value ? <ValueView engine={subject.engine} value={subject.value} /> : null,
  }));
}

/**
 * What the client observed while the call was made.
 *
 * A retained trace is in the value itself; a traced node that is still running has one only the
 * engine can answer for, which is what the live inspection keeps asking it.
 */
const traceView: ResultView = {
  name: "trace",
  matches: ({ value, node }) => (value !== undefined && httpTrace(value.data) !== undefined) || node?.traced === true,
  Draw: ({ subject }) => {
    const { value, node, engine, generation } = subject;
    if (value && httpTrace(value.data)) return <HttpInspection data={value.data} />;
    if (engine && node?.traced) {
      return <LiveHttpInspection key={`${generation}:${node.id}:${node.run}`} engine={engine} node={node.id} run={node.run} state={node.state} />;
    }
    return null;
  },
};

/**
 * The command as it was written — any command, so ⌘click on any command row reaches one; but
 * pointed at from the cell only when the row could not show it whole.
 */
const sourceView: ResultView = {
  name: "source",
  matches: ({ node }) => node !== undefined && node.command.trim() !== "",
  Draw: ({ subject }) => (subject.node ? <SourceView key={subject.node.command} source={subject.node.command} /> : null),
};

/** Most particular first: an HTTP response outranks the trace of the call that made it. */
const supplementaryViews = [traceView, sourceView];
export const RESULT_VIEWS: readonly ResultView[] = [...moduleViews(), ...supplementaryViews];

export const RESULT_VIEW_NAMES: readonly string[] = RESULT_VIEWS.map((view) => view.name);

/** Every view this result admits, in the registry's order. */
export function viewsFor(subject: ViewSubject): readonly ResultView[] {
  return [...moduleViews(), ...supplementaryViews].filter((view) => view.matches(subject));
}

export function viewNamed(name: string | undefined): ResultView | undefined {
  return [...moduleViews(), ...supplementaryViews].find((view) => view.name === name);
}
