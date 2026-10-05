import { afterEach, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { SourceView } from "./SourceView";

const { makeSourceViewer, pack } = vi.hoisted(() => ({ makeSourceViewer: vi.fn(), pack: {} }));
vi.mock("./calc-editor", () => ({ makeSourceViewer }));
vi.mock("./language", () => ({ language: () => Promise.resolve(pack) }));
let tree: ReactTestRenderer | undefined;
afterEach(() => { act(() => tree?.unmount()); tree = undefined; vi.resetAllMocks(); vi.unstubAllGlobals(); });

it("mounts the exact source, replaces changed source and releases both viewer instances", async () => {
  const first = { destroy: vi.fn(), focus: vi.fn() }, second = { destroy: vi.fn(), focus: vi.fn() };
  makeSourceViewer.mockReturnValueOnce(first).mockReturnValueOnce(second);
  vi.stubGlobal("document", { activeElement: null });
  const source = ':calc {\n\treturn "untouched  text";\n}';
  await act(async () => {
    tree = create(<SourceView source={source} onClose={() => {}} />, {
      createNodeMock: node => node.props.className === "cell-source-code" ? {} : null,
    });
  });
  expect(makeSourceViewer).toHaveBeenLastCalledWith(expect.any(Object), source, pack);
  expect(tree!.root.findAllByType("textarea")).toHaveLength(0);
  const changed = ":calc {\n\n  return 2;\n}";
  await act(async () => tree!.update(<SourceView source={changed} onClose={() => {}} />));
  expect(first.destroy).toHaveBeenCalledOnce();
  expect(makeSourceViewer).toHaveBeenLastCalledWith(expect.any(Object), changed, pack);
  act(() => tree!.unmount()); tree = undefined;
  expect(second.destroy).toHaveBeenCalledOnce();
});

it("keeps all lines readable and noneditable if the optional viewer fails to mount", async () => {
  makeSourceViewer.mockImplementation(() => { throw new Error("synthetic load failure"); });
  vi.stubGlobal("document", { activeElement: null });
  const source = [":calc {", ...Array.from({ length: 30 }, (_, n) => `  // ${n}`), "  return 1;", "}"].join("\n");
  await act(async () => {
    tree = create(<SourceView source={source} onClose={() => {}} />, { createNodeMock: () => ({}) });
  });
  const field = tree!.root.findByType("textarea");
  expect(field.props.value).toBe(source);
  expect(field.props.readOnly).toBe(true);
  expect(field.props.rows).toBe(12);
});

const key = (over: Partial<{ key: string; metaKey: boolean; ctrlKey: boolean }>) =>
  ({ key: "a", metaKey: false, ctrlKey: false, preventDefault: vi.fn(), stopPropagation: vi.fn(), ...over });
const mount = async (onClose?: () => void) => {
  makeSourceViewer.mockReturnValue({ destroy: vi.fn(), focus: vi.fn() });
  vi.stubGlobal("document", { activeElement: null });
  await act(async () => {
    tree = create(<SourceView source=":calc { return 1; }" head={false} onClose={onClose} />, {
      createNodeMock: node => node.props.className === "cell-source-code" ? {} : null,
    });
  });
  return tree!.root.findByProps({ role: "group" });
};

it("should_LetEscapeReachTheScreen_When_TheViewHasNothingToClose", async () => {
  // Arrange: a peek shows the source; the screen owns Escape
  const view = await mount(undefined);
  const escape = key({ key: "Escape" });
  // Act
  act(() => view.props.onKeyDown(escape));
  // Assert
  expect(escape.stopPropagation).not.toHaveBeenCalled();
  expect(escape.preventDefault).not.toHaveBeenCalled();
});

it("should_CloseItselfAndStopEscape_When_ItWasGivenAClose", async () => {
  // Arrange
  const onClose = vi.fn();
  const view = await mount(onClose);
  const escape = key({ key: "Escape" });
  // Act
  act(() => view.props.onKeyDown(escape));
  // Assert
  expect(onClose).toHaveBeenCalledOnce();
  expect(escape.stopPropagation).toHaveBeenCalledOnce();
});

it("should_StopOnlyTheSubmitAndRepeatChords_When_OtherKeysArePressed", async () => {
  // Arrange
  const view = await mount(undefined);
  const submit = key({ key: "Enter", metaKey: true });
  const plain = key({ key: "j" });
  // Act
  act(() => view.props.onKeyDown(submit));
  act(() => view.props.onKeyDown(plain));
  // Assert
  expect(submit.preventDefault).toHaveBeenCalledOnce();
  expect(submit.stopPropagation).toHaveBeenCalledOnce();
  expect(plain.stopPropagation).not.toHaveBeenCalled();
});
