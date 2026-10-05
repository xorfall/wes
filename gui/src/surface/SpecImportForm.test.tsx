import { useState } from "react";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SpecImportForm, type ImportHold } from "./SpecImportForm";

/* Synthetic workspace, paths and endpoints only. */

let tree: ReactTestRenderer | undefined;
afterEach(() => { if (tree) act(() => tree!.unmount()); tree = undefined; });

const text = (node: ReactTestInstance | string): string => typeof node === "string" ? node : node.children.map(text).join("");
const byLabel = (label: string) => tree!.root.findByProps({ "aria-label": label });
const submitButton = () => tree!.root.findAllByType("button").find(b => text(b) === "import")!;

function Harness({ holds = [], submit, workspace }: { holds?: ImportHold[]; submit: (command: string) => void; workspace?: string }) {
  const [alias, setAlias] = useState("posts");
  const [endpoint, setEndpoint] = useState("");
  const [replace, setReplace] = useState(false);
  return <SpecImportForm name="placeholder" kind="draft" revision="r3" state={{ text: "ready to import", tone: "mono-ok" }} workspace={workspace} descriptorPath="/library/placeholder/r3.json"
    holds={holds} busy={false} alias={alias} onAlias={setAlias} endpoint={endpoint} onEndpoint={setEndpoint} replace={replace} onReplace={setReplace} servers={1} onSubmit={submit} />;
}

describe("the import form", () => {
  it("should_ShowEachProblemUnderItsOwnField_And_HoldSubmission_When_InputIsInvalid", () => {
    const submit = vi.fn();
    act(() => { tree = create(<Harness submit={submit} />); });
    // Nothing is flagged before the person types or leaves the endpoint.
    expect(tree!.root.findAllByProps({ className: "spec-import-error mono-bad" })).toHaveLength(0);
    expect(submitButton().props.disabled).toBe(true);
    expect(JSON.stringify(tree!.toJSON())).toContain("type the endpoint to import");
    act(() => byLabel("Import alias").props.onChange({ target: { value: "posts-v2" } }));
    act(() => byLabel("Import endpoint").props.onChange({ target: { value: "https://synthetic.invalid/?v=1" } }));
    const alias = byLabel("Import alias");
    const endpoint = byLabel("Import endpoint");
    expect(alias.props["aria-invalid"]).toBe(true);
    expect(text(tree!.root.findByProps({ id: alias.props["aria-describedby"] }))).toBe("Use letters, digits and _; start with a letter or _.");
    expect(text(tree!.root.findByProps({ id: endpoint.props["aria-describedby"] }))).toBe("Remove the query part; an endpoint is scheme, host and path only.");
    expect(JSON.stringify(tree!.toJSON())).toContain("fix the 2 fields above");
    expect(submitButton().props.disabled).toBe(true);
    act(() => tree!.root.findByType("form").props.onSubmit({ preventDefault() {} }));
    expect(submit).not.toHaveBeenCalled();
  });

  it("should_FlagAnEmptyEndpointOnlyAfterItIsLeft", () => {
    act(() => { tree = create(<Harness submit={vi.fn()} />); });
    act(() => byLabel("Import endpoint").props.onBlur());
    expect(byLabel("Import endpoint").props["aria-invalid"]).toBe(true);
    expect(JSON.stringify(tree!.toJSON())).toContain("Type the endpoint the snapshot will call.");
  });

  it("should_SubmitTheExactCommandWithReplaceFalse_And_NeverOfferAnEnvironmentChoice", () => {
    const submit = vi.fn();
    act(() => { tree = create(<Harness submit={submit} workspace="synthetic-lab" />); });
    expect(tree!.root.findAllByType("select")).toHaveLength(0);
    const into = tree!.root.findByProps({ className: "spec-import-into" });
    expect(text(into)).toBe("synthetic-labthe session’s current environment · change it in /env");
    const replace = tree!.root.findAllByType("input").find(i => i.props.type === "checkbox")!;
    expect(replace.props.checked).toBe(false);
    // The document's server is mentioned, never filled in.
    expect(byLabel("Import endpoint").props.value).toBe("");
    expect(JSON.stringify(tree!.toJSON())).toContain("used only if you type it here");
    act(() => byLabel("Import alias").props.onChange({ target: { value: "_posts" } }));
    act(() => byLabel("Import endpoint").props.onChange({ target: { value: "https://synthetic.invalid/v1" } }));
    expect(text(tree!.root.findByProps({ className: "spec-command mono-meta" }))).toBe(':import spec file:"/library/placeholder/r3.json" as:_posts endpoint:"https://synthetic.invalid/v1" replace:false');
    expect(submitButton().props.disabled).toBe(false);
    act(() => tree!.root.findByType("form").props.onSubmit({ preventDefault() {} }));
    expect(submit).toHaveBeenCalledExactlyOnceWith(':import spec file:"/library/placeholder/r3.json" as:_posts endpoint:"https://synthetic.invalid/v1" replace:false');
    // The command is secondary: folded into technical details, below the primary fields.
    expect(tree!.root.findByProps({ className: "spec-command mono-meta" }).parent!.parent!.type).toBe("details");
  });

  it("should_KeepTypedValuesReadOnly_When_Held_And_ReturnThemWhenTheHoldClears", () => {
    const submit = vi.fn();
    let hold!: (held: boolean) => void;
    function Toggle() {
      const [held, setHeld] = useState(false);
      hold = setHeld;
      return <Harness submit={submit} holds={held ? [{ title: "Import held.", detail: "Import uses saved text only; your edits after r3 aren’t saved.", tone: "mono-warn" }] : []} />;
    }
    act(() => { tree = create(<Toggle />); });
    act(() => byLabel("Import endpoint").props.onChange({ target: { value: "https://synthetic.invalid/v1" } }));
    act(() => hold(true));
    expect(tree!.root.findAllByProps({ "aria-label": "Import endpoint" })).toHaveLength(0);
    expect(JSON.stringify(tree!.toJSON())).toContain("https://synthetic.invalid/v1");
    expect(text(tree!.root.findByProps({ className: "spec-command mono-meta" }))).toBe("");
    expect(submitButton().props.disabled).toBe(true);
    act(() => tree!.root.findByType("form").props.onSubmit({ preventDefault() {} }));
    expect(submit).not.toHaveBeenCalled();
    act(() => hold(false));
    expect(byLabel("Import endpoint").props.value).toBe("https://synthetic.invalid/v1");
    expect(byLabel("Import alias").props.value).toBe("posts");
  });
});
