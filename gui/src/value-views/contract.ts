import type { ComponentType, ReactNode } from "react";
import type { StoredValue, TypeShape } from "../protocol";
import type { Context, PresentationNode, Run } from "../presentation/types";

/** A view gets data and presentation services, never an engine or a workspace command API. */
export interface ViewInput {
  readonly path: string;
  readonly type: TypeShape;
  readonly data: unknown;
  readonly context: Context;
  /** Stable host identity when rendered as an instance; absent for ordinary value views. */
  readonly instanceKey?: string;
  /**
   * The live frame entry's input revision: a host-side delivery label, never sent to the view and
   * never derived from input values. Linked-input patches are not covered by it.
   */
  readonly inputRevision?: string;
  readonly coordinated?: boolean;
  /** Prepared child views supplied by the workspace host, never copied into the input value. */
  readonly slots?: Readonly<Record<string, readonly PresentationNode[]>>;
}
export interface ViewPresentationHost {
  readonly remaining: () => number;
  spend(lines: number): void;
  child(name: string, type: TypeShape, data: unknown, options?: { readonly view: string }): PresentationNode;
}
export interface ViewPresentation {
  readonly model: unknown;
  readonly children: readonly PresentationNode[];
  readonly ownLines: number;
  readonly summary: readonly Run[];
}
export interface ViewComponentProps {
  readonly model: unknown;
  readonly children: readonly PresentationNode[];
  readonly renderChild: (node: PresentationNode, options?: { readonly interaction: "inherit" }) => ReactNode;
  readonly interaction?: import("./interaction").InteractionPort<unknown, unknown>;
}
export type ViewValue = Pick<StoredValue, "type" | "data">;
export interface ValueViewModule {
  readonly id: string;
  readonly definition?: import("./definition").ViewDefinition;
  readonly outputSnapshot?: (state: unknown) => unknown;
  readonly outputEvents?: (previous:unknown,event:unknown,next:unknown) => readonly {port:string;value:unknown}[];
  readonly interaction?: import("./interaction").InteractionDefinition<unknown, unknown>;
  /** Must check structure; a record name alone is never sufficient. */
  readonly matches: (type: TypeShape, data: unknown) => boolean;
  readonly prepare?: (value: ViewValue, preparedData: unknown) => { data: unknown; pending: boolean };
  readonly prepareAsync?: (value: ViewValue, preparedData: unknown, signal?: AbortSignal) => Promise<{ data: unknown; pending: boolean }>;
  readonly present: (input: ViewInput, host: ViewPresentationHost) => ViewPresentation;
  readonly Component: ComponentType<ViewComponentProps>;
}

/** An intentionally public, bounded explanation of invalid view input. */
export class ViewInputError extends Error {
  constructor(message: string) { super(message.slice(0, 240)); }
}
