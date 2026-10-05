import { yamlLocation, type YamlContext, type YamlSchema } from "./yaml-schema";
import type { TypeVocabulary } from "./yaml-highlight";
import type { Segment } from "./MonoLine";

export interface Candidate {
  readonly text: string;
  readonly label: string;
  readonly detail?: string;
  readonly kind: "type" | "view-kind" | "keyword" | "property";
}

/** Context is the selected /edit buffer, not a guess from a still incomplete document. */
export function completionsAt(source: string, caret: number, vocabulary: TypeVocabulary, limit = 50, context: YamlContext = "types", schema?: YamlSchema): Candidate[] {
  if (!schema) return [];
  const constructors = schema.typeConstructors;
  const constructorNames = constructors.map(item => item.name);
  const place = yamlLocation(source, caret, context, schema);
  if (place.blocked) return [];
  const prefix = place.prefix.toLowerCase();
  const names = place.keys ?? (place.shape.type
    ? [...vocabulary.names.filter(name => !constructorNames.includes(name)), ...constructorNames]
    : place.shape.values ?? []);
  return [...new Set(names)].filter(name => name.toLowerCase().startsWith(prefix)).slice(0, limit).map(name => ({
    text: name,
    label: place.shape.type && constructors.find(item => item.name === name)?.parameters.length ? `${name}<${constructors.find(item => item.name === name)!.parameters.join(", ")}>` : name,
    detail: place.keys ? place.keyTypes?.[name] : place.shape.expected,
    kind: place.keys ? "property" : place.shape.type ? "type" : "keyword",
  }));
}

export function candidateLine(candidate: Candidate, chosen: boolean, width = 20): Segment[] {
  const label = candidate.label + " ".repeat(Math.max(1, width - candidate.label.length));
  const line: Segment[] = [{ text: chosen ? "› " : "  ", role: "mono-ref" }, { text: label, role: chosen ? "mono-provider" : "mono-dim" }];
  if (candidate.detail) line.push({ text: "· ", role: "mono-faint" }, { text: candidate.detail, role: "mono-faint" });
  return line;
}
