import type { ValueViewModule } from "../../value-views/contract";

/** Deliberately not HTTP: proves that the host needs no knowledge of the next view. */
export const durationView: ValueViewModule = {
  id: "duration-demo",
  matches(type, data) {
    return type.kind === "record" && type.fields?.some(f => f.name === "milliseconds" && f.type.kind === "primitive" && f.type.name === "INT") === true
      && typeof data === "object" && data !== null && "milliseconds" in data && typeof data.milliseconds === "number";
  },
  present({ data, context }, host) {
    const milliseconds = (data as { milliseconds: number }).milliseconds;
    host.spend(1);
    return { model: { seconds: milliseconds / 1000, detailed: context.mode !== "preview" }, children: [], ownLines: 1,
      summary: [{ text: `${milliseconds} ms`, tone: "dim" }] };
  },
  Component({ model }) {
    const value = model as { seconds: number; detailed: boolean };
    return <div className="mono-param">{value.seconds} seconds{value.detailed && <span className="mono-dim"> · elapsed duration</span>}</div>;
  },
};
