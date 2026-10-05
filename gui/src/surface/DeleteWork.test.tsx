import { afterEach, expect, it, vi } from "vitest";
import { createRef } from "react";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { DeleteWork, publicText, type DeleteWorkActions, type DeleteWorkHandle } from "./DeleteWork";
import { Cell } from "./Cell";
import { StorageError } from "../storage-error";
import type { WorkPreview } from "../protocol";

const ID_A = "3f2a9c1e-7b4d-4e8a-9c21-0d5e6f7a8b9c";
const ID_B = "a1b2c3d4-0000-4111-8222-333344445555";
const ID_C = "0badcafe-1111-4222-9333-444455556666";
const preview: WorkPreview = {
  token: "reviewed", cells: [ID_A], nodes: ["id1000"], dependents: [],
  labels: { [ID_A]: ":calc 1" }, payloads: ["result-a"], protected: [], sharedWorkspaces: [], expiresInSeconds: 120,
};
let tree: ReactTestRenderer;
let handle = createRef<DeleteWorkHandle>();
afterEach(() => { if (tree) act(() => tree.unmount()); });
function mount(actions: DeleteWorkActions, onDismiss?: () => void, focus?: () => void) {
  handle = createRef<DeleteWorkHandle>();
  act(() => { tree = create(<DeleteWork ref={handle} actions={actions} onDismiss={onDismiss} />, {
    createNodeMock: node => node.type === "button" ? { focus: focus ?? (() => {}) } : null,
  }); });
}
const button = (label: string) => tree.root.findAllByType("button").find(node => node.children.join("") === label)!;
const open = () => act(async () => { handle.current!.review(); });
async function click(label: string) { await act(async () => { button(label).props.onClick(); }); }
const text = () => JSON.stringify(tree.toJSON());
const words = () => tree.root.findAllByType("pre").map(line => line.findAllByType("span").map(span => span.children.join("")).join("")).join("\n");

it("draws nothing until the cell asks for a review", () => {
  mount({ preview: vi.fn(async () => preview), confirm: vi.fn(async () => {}) });
  expect(tree.toJSON()).toBeNull();
});

it("previews without deleting, then confirms exactly the reviewed authority once", async () => {
  let finish!: () => void;
  const actions = { preview: vi.fn(async () => preview), confirm: vi.fn(() => new Promise<void>(resolve => { finish = resolve; })) };
  mount(actions);
  await open();
  expect(actions.confirm).not.toHaveBeenCalled();
  expect(words()).toContain("$id1000");
  const confirm = button("Delete work").props.onClick;
  await act(async () => { confirm(); confirm(); });
  expect(actions.confirm).toHaveBeenCalledExactlyOnceWith("reviewed", false, false);
  expect(button("Close").props.disabled).toBe(true);
  await act(async () => finish());
  expect(text()).toContain("Waiting for the workspace update");
});

it("opens once: asking again while the review is on screen keeps its choices", async () => {
  const actions = { preview: vi.fn(async () => ({ ...preview, protected: ["p"] })), confirm: vi.fn(async () => {}) };
  mount(actions); await open();
  act(() => tree.root.findByType("input").props.onChange({ currentTarget: { checked: true } }));
  await open();
  expect(actions.preview).toHaveBeenCalledOnce();
  expect(tree.root.findByType("input").props.checked).toBe(true);
});

it("moves focus to the safe choice when the review or its failure appears", async () => {
  const focus = vi.fn();
  mount({ preview: async () => preview, confirm: async () => {} }, undefined, focus);
  await open();
  expect(focus).toHaveBeenCalled();
});

it("discloses grouped attempts, dependents and shared workspaces, requiring explicit protected consent", async () => {
  const actions = { preview: vi.fn(async () => ({ ...preview, cells: [ID_A, ID_B, ID_C],
    nodes: ["id1000", "id1001"], dependents: ["d1-0000-0000-0000-000000000001", "d2"],
    labels: { [ID_A]: ":calc 1", [ID_B]: ":calc 1", [ID_C]: ":calc $id1000\nsecond line" },
    sharedWorkspaces: ["other-workspace"], protected: ["protected-result"] })), confirm: vi.fn(async () => {}) };
  mount(actions); await open();
  const shown = words();
  expect(shown).toContain("Delete this work and 2 related attempts?");
  expect(shown).toContain(":calc 1 ×2");
  expect(shown).toContain(":calc $id1000");
  expect(shown).not.toContain("second line");
  expect(shown).toContain("$id1000 $id1001");
  expect(shown).toContain("Also removes 2 dependent command groups.");
  expect(shown).toContain("other-workspace");
  expect(shown).toContain("Completed external effects are not undone");
  expect(button("Delete work").props.disabled).toBe(true);
  await click("Delete work"); expect(actions.confirm).not.toHaveBeenCalled();
  act(() => tree.root.findByType("input").props.onChange({ currentTarget: { checked: true } }));
  await click("Delete work");
  // Grouped attempts require additional-work consent even if `dependents` is empty.
  expect(actions.confirm).toHaveBeenCalledExactlyOnceWith("reviewed", true, true);
});

it("names only a handful of commands and counts the rest", async () => {
  const cells = ["aaaaaaaa", "bbbbbbbb", "cccccccc", "dddddddd", "eeeeeeee", "ffffffff", "gggggggg"];
  mount({ preview: async () => ({ ...preview, cells, labels: Object.fromEntries(cells.map((cell, at) => [cell, `:calc ${at}`])) }),
    confirm: async () => {} });
  await open();
  expect(words()).toContain(":calc 4"); expect(words()).not.toContain(":calc 5"); expect(words()).toContain("+2 more");
});

it("never shows a cell id, even without a label, for dependents or in engine text", async () => {
  const actions = { preview: vi.fn(async () => ({ ...preview, cells: [ID_A, ID_B], labels: { [ID_A]: ":calc 1" },
    dependents: [ID_C] })), confirm: vi.fn(async () => { throw new Error(`cell ${ID_A} changed; ${ID_B} was not deleted`); }) };
  mount(actions); await open();
  expect(words()).toContain("command without a description");
  expect(words()).toContain("Also removes 1 dependent command group.");
  await click("Delete work");
  const shown = text() + words();
  for (const id of [ID_A, ID_B, ID_C]) expect(shown).not.toContain(id);
  expect(words()).toContain("a command changed; a command was not deleted");
});

it("shows backend blockers without cell ids, confirmation or optimistic deletion", async () => {
  const actions = { preview: vi.fn(async () => { throw new StorageError(`No work deleted for ${ID_A}.`, [
    { node: "id1001", cells: [ID_B], state: "Pending", reason: `execution of ${ID_B} not completed` },
    { node: null, cells: ["incoming"], state: "Preparing", reason: "admission pending" },
  ]); }), confirm: vi.fn(async () => {}) };
  mount(actions); await open();
  expect(words()).toContain("$id1001 · Pending · execution of a command not completed");
  expect(words()).toContain("submission · Preparing · admission pending");
  for (const id of [ID_A, ID_B]) expect(text()).not.toContain(id);
  expect(tree.root.findAllByProps({ "aria-label": "Review cell deletion" })).toHaveLength(0);
  expect(actions.confirm).not.toHaveBeenCalled();
});

it.each(["Reviewed work changed; nothing deleted.", "Deletion completion is unknown; inspect before requesting a fresh preview."])(
  "consumes a failed confirmation without retry: %s", async message => {
    const actions = { preview: vi.fn(async () => preview), confirm: vi.fn(async () => { throw new Error(message); }) };
    mount(actions); await open(); await click("Delete work");
    expect(text()).toContain(message);
    expect(button("Delete work")).toBeUndefined();
    expect(actions.preview).toHaveBeenCalledOnce(); expect(actions.confirm).toHaveBeenCalledOnce();
    await click("Review again");
    expect(actions.preview).toHaveBeenCalledTimes(2); expect(actions.confirm).toHaveBeenCalledOnce();
  });

it("closing a pending preview discards its late reply, does not delete and lets it be reviewed again", async () => {
  let finish!: (value: WorkPreview) => void;
  const dismissed = vi.fn();
  const actions = { preview: vi.fn(() => new Promise<WorkPreview>(resolve => { finish = resolve; })), confirm: vi.fn(async () => {}) };
  mount(actions, dismissed); await open(); await click("Close");
  await act(async () => finish(preview));
  expect(tree.toJSON()).toBeNull(); expect(dismissed).toHaveBeenCalledOnce();
  expect(actions.confirm).not.toHaveBeenCalled();
  await open();
  expect(actions.preview).toHaveBeenCalledTimes(2);
});

it("reaches the review from a collapsed cell through the cell's own key, and a new attempt discards approval", async () => {
  const actions = { preview: vi.fn(async () => preview), confirm: vi.fn(async () => {}) };
  const cell = (attempt: string) => <Cell theme="keys" state="default" rows={[]} verdict={[]} view="collapsed" attempt={attempt} actions={{ deleteWork: actions }} />;
  act(() => { tree = create(cell("first")); });
  await act(async () => tree.root.findByType("section").props.onKeyDown({
    key: "D", shiftKey: true, repeat: false, metaKey: false, ctrlKey: false, altKey: false, preventDefault() {}, stopPropagation() {},
  }));
  expect(button("Delete work")).toBeDefined(); expect(actions.confirm).not.toHaveBeenCalled();
  act(() => tree.update(cell("second")));
  expect(tree.root.findAllByProps({ className: "cell-deletion" })).toHaveLength(0);
  expect(button("Delete work")).toBeUndefined();
});

it("keeps question keyboard events from triggering the containing cell's actions, and Escape gives the cell focus back", async () => {
  const dismissed = vi.fn();
  mount({ preview: async () => preview, confirm: async () => {} }, dismissed); await open();
  const stopPropagation = vi.fn(); const preventDefault = vi.fn();
  act(() => tree.root.findByProps({ className: "cell-deletion" }).props.onKeyDown({ key: "Escape", stopPropagation, preventDefault }));
  expect(stopPropagation).toHaveBeenCalledOnce(); expect(preventDefault).toHaveBeenCalledOnce();
  expect(tree.toJSON()).toBeNull(); expect(dismissed).toHaveBeenCalledOnce();
});

it("rewrites engine ids but leaves ordinary words and short names alone", () => {
  expect(publicText(`cell ${ID_A} is busy`)).toBe("a command is busy");
  expect(publicText("attempt-1234 stays", ["attempt-1234"])).toBe("a command stays");
  expect(publicText("id1 is a name", ["id1"])).toBe("id1 is a name");
});
