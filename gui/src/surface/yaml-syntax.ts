import { parseDocument } from "yaml";

/** Syntax only. Package fields, types and domain rules remain engine-owned. */
export function yamlSyntaxProblem(source: string): string | undefined {
  if (new TextEncoder().encode(source).byteLength > 1_048_576) return "YAML exceeds 1 MiB of UTF-8 input.";
  try {
    // Inspect the syntax tree without materializing values or expanding aliases.
    const document = parseDocument(source, { prettyErrors: true, uniqueKeys: true });
    return document.errors[0]?.message;
  } catch (error) {
    return error instanceof Error ? error.message : "Could not parse YAML.";
  }
}
