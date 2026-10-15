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
import { ValueView } from "../views/Result";
import { valueViewModules } from "../value-views/registry";
import { SourceView } from "./SourceView";
import {preparedValues} from "./render/ValueBlock";
import { registryStore } from "../presentation/registry-store";

/**
 * What there is to look at: the node the engine reported, the value read back when it has been,
 * and the engine a value's own view reads further through.
 */
export interface ViewSubject {
  readonly value?: StoredValue;
  readonly node?: WorkspaceNode;
  readonly engine?: Engine;
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
 * The command as it was written — any command, so ⌘click on any command row reaches one; but
 * pointed at from the cell only when the row could not show it whole.
 */
const sourceView: ResultView = {
  name: "source",
  matches: ({ node }) => node !== undefined && node.command.trim() !== "",
  Draw: ({ subject }) => (subject.node ? <SourceView key={subject.node.command} source={subject.node.command} /> : null),
};

/** After the value's own views: a typed result outranks the command that produced it. */
const supplementaryViews = [sourceView];
export const RESULT_VIEWS: readonly ResultView[] = [...moduleViews(), ...supplementaryViews];

export const RESULT_VIEW_NAMES: readonly string[] = RESULT_VIEWS.map((view) => view.name);

/** Every view this result admits, in the registry's order. */
export function viewsFor(subject: ViewSubject): readonly ResultView[] {
  return [...moduleViews(), ...supplementaryViews].filter((view) => view.matches(subject));
}

export function viewNamed(name: string | undefined): ResultView | undefined {
  return [...moduleViews(), ...supplementaryViews].find((view) => view.name === name);
}
