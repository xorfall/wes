import test from "node:test";
import assert from "node:assert/strict";
import { generateContract } from "./contracts.mjs";

/* Synthetic definition: an invented View with one read-only Dataset input field. */
const none = { min: null, max: null, minLength: null, maxLength: null, minItems: null, maxItems: null, patterns: [], enum: [] };
const definition = {
  name: "Events", id: "synthetic.events", digest: "sha256:synthetic", input: "Input", outputs: {}, interaction: null,
  contracts: {
    Input: { kind: "record", fields: { events: { type: "Events", optional: false } }, constraints: none },
    Events: { kind: "dataset", element: "Row", constraints: none },
    Row: { kind: "record", fields: { label: { type: "Text", optional: false } }, constraints: none },
    Text: { kind: "scalar", primitive: "Text", constraints: none },
  },
};

test("a Dataset input field is typed as a read-only descriptor of its declared rows", () => {
  const generated = generateContract(definition);
  assert.match(generated, /import type \{ DatasetRef \} from "@wes\/view-sdk";/);
  assert.match(generated, /type T1 = DatasetRef<T2>;/);
  assert.match(generated, /readonly "events": T1;/);
});

test("contracts without a Dataset import nothing for it", () => {
  const plain = { ...definition, contracts: { Input: { kind: "record", fields: {}, constraints: none } } };
  assert.doesNotMatch(generateContract(plain), /DatasetRef/);
});
