import { defineInteractiveView } from "../../value-views/interactive";
import type {StoredValue} from "../../protocol";
import type {InteractionProtocol} from "../../value-views/interaction";
const object = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null;
interface State { readonly count: number }
interface Event { readonly kind: "increment" }
const protocol: InteractionProtocol<State, Event> = {
  id: "counter-example",
  state: (v): v is State => object(v) && Number.isSafeInteger(v.count) && (v.count as number) >= 0,
  event: (v): v is Event => object(v) && v.kind === "increment",
};
export const counterView = defineInteractiveView<null, State, Event>({
  id: "counter-demo",
  matches: (type, data) => type.kind === "record" && type.fields.some(f => f.name === "view" && f.type.kind === "primitive" && f.type.name === "TEXT") && object(data) && data.view === "counter-demo",
  interaction: { protocol, initial: () => ({ count: 0 }), reduce: state => ({ count: state.count + 1 }) },
  present: (_, host) => { host.spend(1); return { model: null, children: [], ownLines: 1, summary: [] }; },
  Component: ({ interaction }) => <button aria-label="Increment local counter" onClick={() => interaction.emit({ kind: "increment" })}>Count {interaction.state.count}</button>,
});
export const counterFixture: { id: string; label: string; value: StoredValue } = {
  id: "counter", label: "Interactive module", value: {
    type: { kind: "record", name: "CounterExample", fields: [{ name: "view", type: { kind: "primitive", name: "TEXT" } }] }, data: { view: "counter-demo" }, provenance: {},
  },
};
