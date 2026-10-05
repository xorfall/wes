/** Match the source language string escapes; never interpolate raw YAML into commands. */
function quoted(value: string): string {
  return `"${value.replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/\n/g, "\\n").replace(/\r/g, "\\r").replace(/\t/g, "\\t")}"`;
}

export function documentCommand(context: "env" | "types", origin: string, base: string, planName: string): string {
  return context === "env"
    ? `:env plan source:"" origin:${quoted(origin)}${base ? ` base:${quoted(base)}` : ""} > ${planName}`
    : `:package load source:"" origin:${quoted(origin)}`;
}

/** Decode only the compact commands this editor generates, never arbitrary source. */
export function documentOperation(text: string): { context: "env" | "types"; origin: string; base: string; planName?: string } | undefined {
  const quotedToken = '"(?:[^"\\\\]|\\\\["\\\\nrt])*"';
  const env = new RegExp(`^:env plan source:"" origin:(${quotedToken})(?: base:(${quotedToken}))? > ([A-Za-z_][A-Za-z0-9_]*)$`).exec(text);
  const types = new RegExp(`^:package load source:"" origin:(${quotedToken})$`).exec(text);
  const match = env ?? types;
  if (!match) return undefined;
  try {
    return { context: env ? "env" : "types", origin: JSON.parse(match[1]!), base: env?.[2] ? JSON.parse(env[2]) : "", ...(env ? { planName: env[3]! } : {}) };
  } catch { return undefined; }
}

export function documentLabel(text: string): string {
  const operation = documentOperation(text);
  if (!operation) return text;
  return operation.context === "env" ? `:env plan > ${operation.planName}` : ":package load";
}
