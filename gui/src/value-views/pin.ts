import type { OutputPort, ViewInstance } from "./instances";

/** The engine's binding grammar; a name outside it is refused rather than bound. */
const BINDING = /^[\p{L}\p{Nd}_]+$/u;
const PIN_SUFFIX = "_pin";
const MAX_PIN_CANDIDATES = 999;

/**
 * A result name nobody in this workspace uses yet. The engine refuses a Pin into a bound name (NAM003);
 * `taken` holds the names and ids of the live nodes the engine has announced, so that refusal is rare.
 */
export function freshPinName(view: string, taken: ReadonlySet<string>): string {
  const base = BINDING.test(view) ? `${view}${PIN_SUFFIX}` : `pinned${PIN_SUFFIX}`;
  for (let at = 1; at <= MAX_PIN_CANDIDATES; at++) {
    const name = at === 1 ? base : `${base}${at}`;
    if (!taken.has(name)) return name;
  }
  throw new Error("No unused result name is available for this Pin; name one with :view pin … > name.");
}

/** Why the displayed input cannot be pinned, or nothing when it can. Mirrors refusals the engine repeats. */
export function pinRefusal(entry: ViewInstance): string | undefined {
  if (entry.linkedInputs.length) return "Pin is not available for views with linked input fields yet. Nothing was saved.";
  if (entry.inputReference.kind === "retained") return "This view already shows a pinned input.";
  if (!entry.input) return entry.query ? "Apply the query first; there is no displayed input to pin." : "This view has no displayed input to pin.";
  if (entry.inputProblem) return `Resolve the input problem before pinning: ${entry.inputProblem}`;
  return undefined;
}

/** The Pin command for exactly the frame on screen: instance, configuration and input revision guards. */
export function pinCommand(entry: Pick<ViewInstance, "id" | "instance" | "revision" | "inputRevision">, name: string, encode: (text: string) => string): string {
  if (!BINDING.test(entry.id) || !BINDING.test(name) || !/^\d+$/.test(entry.revision) || !/^\d+$/.test(entry.inputRevision)) {
    throw new Error("View reference changed; reopen it before pinning its input.");
  }
  return `:view pin $${entry.id} instance:${encode(entry.instance)} revision:${entry.revision} inputRevision:${entry.inputRevision} > ${name}`;
}

/** The data port is the node itself; error and cancel are named, never conflated. */
const PORT_SUFFIX: Readonly<Record<OutputPort, string>> = { data: "", error: ".error", cancel: ".cancel" };

function path(node: string, port: OutputPort, fields: readonly string[]): string {
  return `$${node}${PORT_SUFFIX[port]}${fields.map(field => `.${field}`).join("")}`;
}

/** Says what the input run is for a stream window, where a derived input's run changes with each update. */
export const INPUT_RUN_DESCRIPTION = "Run that produced the displayed input. For derived inputs, this can change with each update; it is not the stream connection ID.";

/**
 * `Current · $orders.body · run r7` or `Pinned · $orders_pin · from $orders.body run r7`. A stream window keeps
 * its label stable and returns the shown run separately as `inputRun`; the frame carries no upstream identity.
 */
export function referenceLabel(entry: Pick<ViewInstance, "inputReference" | "inputDelivery" | "input" | "query">): { readonly kind: string; readonly detail: string; readonly inputRun?: string } {
  const { inputReference: reference, inputDelivery: delivery } = entry;
  switch (reference.kind) {
    // Unlinked holds a literal, a query's own materialized input, or nothing after its source was removed.
    case "unlinked": return entry.query ? { kind: "Query input", detail: entry.input ? `from template ${entry.query.template}` : "not applied yet" }
      : entry.input ? { kind: "Literal input", detail: "not linked to a result" } : { kind: "No input", detail: "not linked to a result" };
    case "current": {
      const source = path(reference.node, reference.port, reference.fields);
      if (delivery === "window") return reference.shownRun
        ? { kind: "Current", detail: `${source} · stream window`, inputRun: reference.shownRun }
        : { kind: "Current", detail: `${source} · stream window · no input run yet` };
      return { kind: "Current", detail: `${source} · ${reference.shownRun ? `shown run ${reference.shownRun}` : "no run shown yet"}` };
    }
    case "retained": return { kind: "Pinned", detail: [`$${reference.node} run ${reference.run}`,
      reference.origin ? `from ${path(reference.origin.node, reference.origin.port, reference.origin.fields)}${reference.origin.run ? ` run ${reference.origin.run}` : ""}` : undefined].filter(Boolean).join(" · ") };
  }
}
