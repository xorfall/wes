/** Generate TypeScript from the canonical resolved Wes contract schema. */
export function generateContract(d, {definitionImport = "@wes/view-sdk", numericImport = definitionImport} = {}) {
  const banner = "// Generated from validated Wes contracts. Do not edit.\n";
  const entries = Object.entries(d.contracts), names = new Map(entries.map(([name],index)=>[name,`T${index}`]));
  const ref = name => { if (!names.has(name)) throw new Error(`Missing contract ${name}`); return names.get(name); };
  const scalar = s => {
    if (s.constraints.enum.length && !["Int","Decimal"].includes(s.primitive)) return s.constraints.enum.map(v=>JSON.stringify(v)).join(" | ");
    return ({Text:"string",Int:"NumericValue",Decimal:"NumericValue",Bool:"boolean",Instant:"string",Duration:"string",Interval:"string",Bytes:"string"})[s.primitive] ?? (()=>{throw new Error(`Unsupported primitive ${s.primitive}`)})();
  };
  const expression = s => {
    switch(s.kind) {
      case "scalar": return scalar(s);
      case "record": return `{ ${Object.entries(s.fields).map(([key,f])=>`readonly ${JSON.stringify(key)}${f.optional?"?":""}: ${ref(f.type)};`).join(" ")} }`;
      case "list": return `readonly ${ref(s.element)}[]`;
      case "option": return `${ref(s.element)} | null`;
      case "union": return s.alternatives.map(ref).join(" | ");
      // A read-only descriptor of one committed snapshot; its rows are read by page, never inlined.
      case "dataset": return `DatasetRef<${ref(s.element)}>`;
      default: throw new Error(`Unsupported contract ${s.kind}`);
    }
  };
  const types = entries.map(([name,s])=>`type ${ref(name)} = ${expression(s)};`).join("\n");
  const outputs = `{ ${Object.entries(d.outputs).filter(([,p])=>p.mode==="state").map(([name,p])=>`readonly ${JSON.stringify(name)}: ${ref(p.type)};`).join(" ")} }`;
  const eventOutputs = `{ ${Object.entries(d.outputs).filter(([,p])=>p.mode==="event").map(([name,p])=>`readonly ${JSON.stringify(name)}: ${ref(p.type)};`).join(" ")} }`;
  const interaction = d.interaction;
  const generated = banner + (types.includes('NumericValue')?`import type { NumericValue } from ${JSON.stringify(numericImport)};\n`:'') + (types.includes('DatasetRef<')?`import type { DatasetRef } from ${JSON.stringify(definitionImport)};\n`:'') + `import type { ViewDefinition } from ${JSON.stringify(definitionImport)};\n` + types +
    `\nexport type Input = ${ref(d.input)};\nexport type Outputs = ${outputs};\nexport type EventOutputs = ${eventOutputs};\nexport type State = ${interaction?ref(interaction.state):"never"};\nexport type Event = ${interaction?ref(interaction.event):"never"};\n` +
    `export const definition: ViewDefinition<Input,Outputs,State,Event,EventOutputs> = ${JSON.stringify(d,null,2)};\n`;
  return generated;
}
