import type { NumericValue } from "./values";

export interface ContractSchema {
  readonly kind: string;
  readonly primitive?: string;
  readonly fields?: Readonly<Record<string, {readonly type: string; readonly optional: boolean}>>;
  readonly element?: string;
  readonly alternatives?: readonly string[];
  readonly constraints: {
    readonly min: string | null; readonly max: string | null;
    readonly minLength: number | null; readonly maxLength: number | null;
    readonly minItems: number | null; readonly maxItems: number | null;
    readonly patterns: readonly string[]; readonly enum: readonly (string | NumericValue | boolean)[];
  };
}
/** Display columns × rows. The host owns containment; a view owns its inner content. */
export interface ViewSize {readonly columns:number;readonly rows:number}
/** Inner frame policy. A dashboard allocates the slot and may override this default. */
export interface ViewPlacement {readonly width:'fill'|'preferred';readonly align:'start'|'center'|'end'}
/** Integer bounds per axis: 1 <= min <= preferred <= max (512 columns, 200 rows).
 * Preview rows guide a bounded summary; they do not crop its natural height. */
export interface ViewTier {readonly min:ViewSize;readonly preferred:ViewSize;readonly max:ViewSize;readonly placement?:ViewPlacement}
export interface ViewLayout {readonly preview:ViewTier;readonly expanded:ViewTier;readonly window:ViewTier}
export interface ViewDefinition<Input = unknown, Outputs = unknown, State = unknown, Event = unknown, EventOutputs = unknown> {
  readonly name: string; readonly id: string; readonly digest: string; readonly summary: string;
  readonly artifact?: string | null;
  readonly layout?: ViewLayout | null;
  readonly inputReferences: readonly ("unlinked" | "current" | "retained")[];
  readonly inputDelivery: readonly ("finite" | "window")[];
  readonly input: string; readonly inputModes: readonly string[];
  readonly outputs: Readonly<Record<string, {readonly type: string; readonly mode: "state" | "event"; readonly shared: boolean}>>;
  readonly outputScope: "local" | "instance";
  readonly interaction: { readonly protocol: string; readonly state: string; readonly event: string; readonly sharedFields: readonly string[] } | null;
  readonly contracts: Readonly<Record<string, ContractSchema>>;
  readonly execution: "none";
  readonly slots: Readonly<Record<string, {readonly accepts: readonly string[]; readonly protocol: string | null; readonly max: number; readonly default: boolean; readonly coordinates: boolean}>>;
  readonly eventWindow: {readonly items:number;readonly bytes:number;readonly read:string;readonly retention:string;readonly overflow:string};
  readonly __types?: {input:Input; outputs:Outputs; state:State; event:Event; eventOutputs:EventOutputs};
}
