import { expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import type { Engine } from "../engine";
import type { Event } from "../protocol";
import { newCell } from "../cells";
import { apply, emptyWorkspace } from "../workspace";
import { cellBlocks } from "./cell-output";
import { readSession } from "./session-model";
import { PeekScreen, peekOf, peekText } from "./screens/Peek";

it("keeps structured issues in cell details, peek and copied text after reconnect", () => {
  const error = { id: "synthetic-http-error", code: "HTTP001", message: "Request contract failed", causeId: "", issues: [
    { path: "/arguments/body/count", code: "TYP005", message: "number is outside Count's bounds" },
    { path: "/arguments/body/mode", code: "TYP005", message: "value is not in Mode's enum" },
  ] };
  const frames: Event[] = [
    { event: "created", dependencyLifetime: "continuous", node: "broken", name: "broken", command: "fixture send", dependsOn: [], interactive: false },
    { event: "failed", node: "broken", reason: error.message, error },
  ];
  for (const workspace of [frames.reduce(apply, emptyWorkspace), frames.reduce(apply, emptyWorkspace)]) {
    const held = new Map();
    const cell = readSession({ workspace, held, cells: [{ ...newCell("fixture send"), state: "answered", nodes: ["broken"] }],
      context: { workspace: "fixture", connection: "connected" } }).cells[0]!;
    const blocks = cellBlocks({ cell, workspace, held, reads: new Map(), retryRead() {}, engine: {} as Engine, generation: "fixture" });
    const html = renderToStaticMarkup(<>{blocks.map(block => block.content)}</>);
    const material = peekOf(workspace.nodes[0], undefined);
    const copied = peekText("error", undefined, material.source, material.failure, undefined, material.failureRecord);
    const peek = renderToStaticMarkup(<PeekScreen top={[]} subject={[]} what="error" {...material} />);
    for (const issue of error.issues) {
      for (const output of [html, peek, copied]) {
        expect(output).toContain(issue.path); expect(output).toContain(issue.code);
        expect(output).toContain(issue.message.replaceAll("'", output === copied ? "'" : "&#x27;"));
      }
    }
  }
});
