import type { ComponentType, ReactNode } from "react";
import type { ViewDefinition } from "./contract";
export type { ViewDefinition, ContractSchema, ViewSize, ViewPlacement, ViewTier, ViewLayout } from "./contract";
export * from "./values";

export type ReadonlyData<T> = T extends object ? Readonly<T> : T;
/** Actual iframe viewport, in CSS pixels and measured host font units. Its height follows
 * natural content under the tier cap, not the dashboard slot's height. Do not feed it
 * back into whole-view height; use width for responsive geometry. */
export interface ViewAllocation {readonly width:number;readonly height:number;readonly columns:number;readonly rows:number}

/** All source observation and command execution belongs to Wes, never the renderer. */
export interface ViewProps<Input, State, Event> {
  readonly input: ReadonlyData<Input>;
  readonly state: ReadonlyData<State>;
  readonly revision: number;
  readonly emit: (event: Event) => void;
  readonly slots: Readonly<Record<string, readonly ReactNode[]>>;
  readonly context: {readonly mode: "preview" | "expanded" | "window"; readonly instance: string | null;readonly allocation?:ViewAllocation; readonly coordinated?: boolean; readonly inspectionOnly?: boolean; readonly inspectionActive?: boolean;
    /** True while the reader's focus is inside this View; a selection may stay but read as inactive. */
    readonly active?: boolean; readonly inspect?: () => void};
}

export interface ViewRenderer<Input, Outputs, State, Event, EventOutputs> {
  readonly Component: ComponentType<ViewProps<Input, State, Event>>;
  /** Optional second presentation of this instance, using the same host controller. */
  readonly Inspection?: ComponentType<ViewProps<Input, State, Event>>;
  readonly initial?: (input: ReadonlyData<Input>) => State;
  readonly reduce?: (state: ReadonlyData<State>, event: Event) => State;
  readonly outputs?: (state: ReadonlyData<State>) => Outputs;
  readonly eventOutputs?: (previous: ReadonlyData<State>, next: ReadonlyData<State>, event: Event) =>
    readonly {[K in keyof EventOutputs]: {readonly port: K; readonly value: EventOutputs[K]}}[keyof EventOutputs][];
}

/** The generated definition carries the exact backend contract; no custom JS validator
 * can replace it. Runtime execution and validation are host responsibilities. */
export function defineView<Input, Outputs, State, Event, EventOutputs>(
  definition: ViewDefinition<Input, Outputs, State, Event, EventOutputs>,
  renderer: ViewRenderer<Input, Outputs, State, Event, EventOutputs>,
) {
  if (definition.interaction && (!renderer.initial || !renderer.reduce || !renderer.outputs)) {
    throw new Error("Interactive views require initial, reduce and outputs");
  }
  if (!definition.interaction && (renderer.initial || renderer.reduce || renderer.outputs || renderer.eventOutputs)) {
    throw new Error("Static views cannot declare interaction callbacks");
  }
  return Object.freeze({definition, ...renderer});
}

export * from "./time";
