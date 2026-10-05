import { parseYamlSchema, type YamlSchema } from "./yaml-schema-protocol";

/** Per-open request: engine changes and failed requests never reuse a cached schema. */
export async function loadYamlSchema(signal: AbortSignal): Promise<YamlSchema> {
  const response = await fetch("/language/yaml", { signal, cache: "no-store" });
  if (!response.ok) throw new Error("YAML completion schema unavailable");
  return parseYamlSchema(await response.json());
}
