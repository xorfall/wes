import { HelpPreview, readHelp } from "../forms/Help";
import { GraphPreview, readGraph } from "../forms/GraphData";
import { MonoLine } from "../MonoLine";


export function CustomDrawing({ name, data }: { readonly name: string; readonly data: unknown }) {
  if (name === "help") return <HelpPreview model={readHelp({ type: { kind: "unknown" }, data })} />;
  if (name === "graph") return <GraphPreview model={readGraph({ type: { kind: "unknown" }, data })} />;
  return <MonoLine segments={[{ text: `no renderer for ${name}`, role: "mono-faint" }]} className="value-line" />;
}
