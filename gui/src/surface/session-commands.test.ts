import { expect, it } from "vitest";
import { read } from "./commands";
import { promptCompletion } from "./prompt-complete";
import { emptyWorkspace } from "../workspace";

it("parses and completes both session commands without forwarding them to the engine", () => {
  for (const name of ["clear", "debug"] as const) {
    expect(read(`/${name}`)).toEqual({ kind: name });
    for (const suffix of ["split", "all", "unexpected"]) expect(read(`/${name} ${suffix}`).kind).toBe("trouble");
    const line = `/${name.slice(0, 2)}`;
    expect(promptCompletion({ line, caret: line.length, catalogue: emptyWorkspace.catalogue, names: [], aliases: {} }).items.map(item => item.text)).toContain(`/${name}`);
  }
});
