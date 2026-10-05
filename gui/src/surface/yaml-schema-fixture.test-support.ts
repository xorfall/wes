/** Test-only publication snapshot; backend parity test checks this exact JSON. */
import published from "./testing/yaml-schema.json";
import { parseYamlSchema } from "./yaml-schema-protocol";
export const schema = parseYamlSchema(published);
