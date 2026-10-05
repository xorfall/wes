import { afterEach, describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { DiagnosticsPanel } from "./DiagnosticsPanel";

/* The process's diagnostics as a bare store: synthetic status, no engine. */
const telemetry = vi.hoisted(() => ({
  status: undefined as unknown,
  listeners: new Set<() => void>(),
  control: vi.fn(async () => undefined),
  refresh: vi.fn(async () => undefined),
}));
vi.mock("./local-telemetry", () => ({
  diagnosticsStatus: () => telemetry.status,
  subscribeDiagnostics: (listener: () => void) => { telemetry.listeners.add(listener); return () => telemetry.listeners.delete(listener); },
  controlDiagnostics: telemetry.control,
  refreshDiagnostics: telemetry.refresh,
}));

const basic = {
  mode: "basic", saved_mode: "basic", forced: false, remaining_ms: 0, writer_stopping: false, previous_unclean: false,
  dropped: 2, suppressed: 0, write_errors: 0, metrics: { operations: [{ operation: "submit", outcomes: [["ok", 3]], sum_us: 4200 }] },
};
let tree: ReactTestRenderer | undefined;
afterEach(() => { act(() => tree?.unmount()); tree = undefined; telemetry.control.mockClear(); telemetry.refresh.mockClear(); });
const draw = (status: unknown) => { telemetry.status = status; act(() => { tree = create(<DiagnosticsPanel />); }); return tree!; };
const text = (shown: ReactTestRenderer) => JSON.stringify(shown.toJSON());

describe("local diagnostics as settings rows", () => {
  it("should_OfferTheCollectionAsCards_And_SetItWhenAnotherIsChosen", async () => {
    // Arrange
    const shown = draw(basic);
    const cards = shown.root.findAllByProps({ role: "radio" }).filter((it) => it.type === "button");
    // Act
    await act(async () => cards.find((it) => it.props["aria-checked"] === false)!.props.onClick());
    // Assert
    expect(cards.map((it) => it.props["aria-checked"])).toEqual([false, true]);
    expect(telemetry.control).toHaveBeenCalledWith({ action: "set", mode: "off" });
  });

  it("should_SayTheStateAsFacts_And_KeepClearForWhenCollectionIsOff", () => {
    // Arrange / Act
    const shown = draw(basic);
    const chips = shown.root.findAllByType("button").filter((it) => String(it.props.className).includes("screen-chip"));
    const byLabel = (label: string) => chips.find((it) => it.children.join("") === label)!;
    // Assert
    expect(text(shown)).toContain("2 dropped");
    expect(byLabel("Clear saved logs").props.disabled).toBe(true);
    expect(byLabel("Capture for 5 minutes").props.disabled).toBe(false);
    expect(byLabel("▸ metrics")).toBeDefined();
  });

  it("should_ShowTheMetricsAsFacts_When_TheChipIsOpened", () => {
    // Arrange
    const shown = draw(basic);
    const chip = shown.root.findAllByType("button").find((it) => it.children.join("") === "▸ metrics")!;
    // Act
    act(() => chip.props.onClick());
    // Assert
    expect(text(shown)).toContain("3 completed");
    expect(text(shown)).toContain("4.2 ms in all");
  });

  it("should_OfferOnlyARefresh_When_NothingHasBeenReadYet", async () => {
    // Arrange
    const shown = draw(undefined);
    const chips = shown.root.findAllByType("button").filter((it) => String(it.props.className).includes("screen-chip"));
    // Act
    await act(async () => chips.find((it) => it.children.join("") === "Refresh diagnostics")!.props.onClick());
    // Assert
    expect(telemetry.refresh).toHaveBeenCalledOnce();
    expect(shown.root.findAllByProps({ role: "radio" }).filter((it) => it.type === "button").every((it) => it.props.disabled)).toBe(true);
  });
});
