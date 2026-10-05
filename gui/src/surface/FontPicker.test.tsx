import { afterEach, describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestInstance, type ReactTestRenderer } from "react-test-renderer";
import { FontPicker } from "./FontPicker";
import { fontCatalogue, fontGroups, resetFontCatalogue } from "./fonts";

let tree: ReactTestRenderer | undefined;
afterEach(() => { if (tree) act(() => tree!.unmount()); tree = undefined; resetFontCatalogue(); vi.unstubAllGlobals(); });
const textOf = (node: ReactTestInstance | string): string => typeof node === "string" ? node : node.children.map(textOf).join("");
const families = [
  { name: "Menlo", monospace: true }, { name: "Monaco", monospace: true }, { name: "PT Mono", monospace: true },
  { name: "Avenir Next", monospace: false }, { name: "Helvetica Neue", monospace: false },
];

describe("font catalogue", () => {
  it("should_OfferOnlyMonospacedOrOnlyProportionalFamilies_When_APickerAsks", () => {
    // Act
    const mono = fontGroups(families, "mono", "PT Mono", () => false);
    const sans = fontGroups(families, "sans", "IBM Plex Sans", () => false);
    // Assert: shipped faces apart, the machine's sorted, never the other kind.
    expect(mono).toEqual({ shipped: ["PT Mono"], installed: ["Menlo", "Monaco"] });
    expect(sans).toEqual({ shipped: ["IBM Plex Sans"], installed: ["Avenir Next", "Helvetica Neue", "system-ui"] });
  });

  it("should_KeepTheChosenFaceAndSystemFaces_When_TheCatalogueDoesNotListThem", () => {
    const mono = fontGroups([], "mono", "JetBrains Mono", (name) => name === "SF Mono");
    expect(mono.installed).toEqual(["JetBrains Mono", "SF Mono"]);
  });

  it("should_DropUnreadableEntries_When_TheCatalogueIsRead", async () => {
    // Arrange
    const answer = { families: [{ name: "Menlo", monospace: true }, { name: 'x"}', monospace: true }, { name: 3 }, { name: "Futura" }] };
    const fetcher = vi.fn(async () => ({ ok: true, json: async () => answer })) as unknown as typeof fetch;
    // Act
    const read = await fontCatalogue(fetcher);
    // Assert
    expect(read).toEqual([{ name: "Menlo", monospace: true }]);
  });

  it("should_AnswerNone_When_TheRouteIsMissing", async () => {
    const fetcher = vi.fn(async () => ({ ok: false, json: async () => ({}) })) as unknown as typeof fetch;
    expect(await fontCatalogue(fetcher)).toEqual([]);
  });
});

describe("font picker", () => {
  it("should_FilterAndPickAFaceDrawnInItself_When_TheListIsOpened", async () => {
    // Arrange
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ families }) })));
    const onPick = vi.fn();
    await act(async () => { tree = create(<FontPicker kind="mono" label="Data font" chosen="PT Mono" onPick={onPick} />); });
    // Act
    act(() => tree!.root.findByProps({ "aria-label": "Data font: PT Mono" }).props.onClick());
    const names = () => tree!.root.findAllByProps({ role: "option" }).map((option) => textOf(option.findByProps({ className: "font-picker-name" })));
    const all = names();
    act(() => tree!.root.findByProps({ "aria-label": "Filter data font" }).props.onChange({ target: { value: "men" } }));
    const filtered = names();
    act(() => tree!.root.findAllByProps({ role: "option" })[0]!.props.onClick());
    // Assert
    expect(all).toEqual(["PT Mono", "Menlo", "Monaco", "SF Mono"]);
    expect(filtered).toEqual(["Menlo"]);
    expect(onPick).toHaveBeenCalledExactlyOnceWith("Menlo");
    expect(tree!.root.findAllByProps({ role: "listbox" })).toHaveLength(0);
  });
});
