import { jsonLanguage } from "@codemirror/lang-json";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { tags } from "@lezer/highlight";

// Presentation only: backend diagnostics remain the authority for draft/spec validity.
export const jsonColors = HighlightStyle.define([
  { tag: tags.propertyName, class: "mono-param" },
  { tag: tags.string, class: "mono-provider" },
  { tag: tags.number, class: "mono-literal" },
  { tag: tags.bool, class: "mono-meta" },
  { tag: tags.null, class: "mono-ref" },
  { tag: tags.punctuation, class: "mono-dim" },
]);

export const jsonSyntax = [jsonLanguage, syntaxHighlighting(jsonColors)];
