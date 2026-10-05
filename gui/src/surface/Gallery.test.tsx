import { afterEach, describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { Gallery, GALLERIES, frameScale, readGalleryRoute } from "./Gallery";

function draw(hash: string): ReactTestRenderer {
  const route = readGalleryRoute(hash)!;
  let tree: ReactTestRenderer | undefined;
  act(() => { tree = create(<Gallery route={route} />); });
  return tree!;
}

function textOf(node: { children: readonly unknown[] }): string {
  return node.children
    .map((child) => (typeof child === "string" ? child : textOf(child as { children: readonly unknown[] })))
    .join("");
}

afterEach(() => vi.unstubAllGlobals());

describe("the gallery route", () => {
  it("should_StayOutOfTheWay_When_TheHashIsNotTheSurfaces", () => {
    expect(readGalleryRoute("")).toBeUndefined();
    expect(readGalleryRoute("#settings")).toBeUndefined();
  });

  it("should_ReadTheScreenAndBothAxes_When_TheHashNamesThem", () => {
    expect(readGalleryRoute("#surface/cells-controls/ink/dense")).toEqual({
      screen: "cells-controls", palette: "ink", density: "dense", framed: false,
    });
  });

  it("should_TakeTheBasePaletteAndDensity_When_TheHashNamesNeither", () => {
    expect(readGalleryRoute("#surface/cells-keys")).toEqual({
      screen: "cells-keys", palette: "paper", density: "normal", framed: false,
    });
  });

  it("should_DrawAtTheDesignsOwnSize_When_TheHashAsksForTheFrame", () => {
    expect(readGalleryRoute("#surface/session/paper/1440x900")?.framed).toBe(true);
    // A screen compared at another size is another screen, so the frame shrinks and the size does not.
    expect(frameScale(1382, 797)).toBeCloseTo(797 / 900, 6);
    expect(frameScale(1920, 1200)).toBe(1);
    expect(frameScale(720, 1200)).toBeCloseTo(0.5, 6);
  });

  it("should_FallBackToTheFirstGallery_When_TheScreenIsNotOne", () => {
    expect(readGalleryRoute("#surface/nonsense")?.screen).toBe("cells-keys");
  });
});

describe("a gallery screen", () => {
  it("should_CarryBothAxesOnTheSurfaceRoot_When_ItIsDrawn", () => {
    const tree = draw("#surface/cells-keys/ink/dense");
    const root = tree.root.findByProps({ className: "wes-terminal surface-terminal gallery" });
    expect(root.props["data-palette"]).toBe("ink");
    expect(root.props["data-density"]).toBe("dense");
    act(() => tree.unmount());
  });

  it("should_DrawSixCellsOnePerState_When_ACellGalleryIsAsked", () => {
    for (const screen of ["cells-keys", "cells-controls"]) {
      const tree = draw(`#surface/${screen}`);
      const cells = tree.root.findAllByType("section").filter(node => node.props["data-cell"] !== undefined);
      expect(cells.map((cell) => String(cell.props["data-state"])), screen)
        .toEqual(["default", "focus", "stale", "failed", "pinned", "live"]);
      expect(new Set(cells.map((cell) => String(cell.props["data-theme"]))), screen)
        .toEqual(new Set([screen.replace("cells-", "")]));
      act(() => tree.unmount());
    }
  });

  it("keeps state, duration and retention in their named columns in every gallery cell", () => {
    const tree = draw("#surface/cells-keys");
    const verdicts=tree.root.findAllByProps({"aria-label":"Result status and type"});
    expect(verdicts.map(row=>["state","duration","retention"].map(slot=>textOf(row.findByProps({className:`cell-slot cell-slot-${slot}`})))))
      .toEqual([["ok","340 ms","kept"],["ok","340 ms","kept"],["ok","","1 stale"],["failed","12 ms","not kept"],["ok","","pinned"],["running","18 s","● live"]]);
    act(() => tree.unmount());
  });

  it("should_DrawThreeFixtureLines_When_TheMonoLinesScreenIsAsked", () => {
    const tree = draw("#surface/mono-lines");
    const lines = tree.root.findAllByType("pre").map(textOf);
    expect(lines).toEqual([
      '09:14  ❯ acme orders.list since:2026-09-01 statis:"open"',
      "failed · unknown parameter · 12 ms · not kept",
      `${" ".repeat(43)}^^^^^^^ no such parameter; the package offers status:`,
    ]);
    act(() => tree.unmount());
  });

  it("should_PresentEveryKindOfValueThroughOnePipeline_When_TheValuesScreenIsAsked", () => {
    const tree = draw("#surface/values");
    const cells = tree.root.findAllByType("section").filter(node => node.props["data-cell"] !== undefined).filter(cell => String(cell.props["aria-label"]).endsWith(" value"));
    expect(cells.map((cell) => String(cell.props["aria-label"]))).toEqual([
      "orders value", "usage value", "customer value", "process value", "http value", "deps value",
    ]);
    const lines = tree.root.findAllByType("pre").map(textOf);
    expect(lines).toContain("412 · 388 · 301 · 97 · 64 · 58 · 12");
    expect(JSON.stringify(tree.toJSON())).toContain("cat: /srv/missing.conf");
    act(() => tree.unmount());
  });

  it("should_DrawTheSessionsFiveCells_When_TheSessionScreenIsAsked", () => {
    const tree = draw("#surface/session");
    const cells = tree.root.findAllByType("section").filter(node => node.props["data-cell"] !== undefined);
    expect(cells.map((cell) => String(cell.props["data-state"])))
      .toEqual(["pinned", "default", "live", "failed", "default"]);
    const lines = tree.root.findAllByType("pre").map(textOf);
    expect(lines[0]).toBe("wes  /  sales-api  ·  connected");
    expect(lines).toContain("~ 2 results stale   /stale");
    expect(lines).toContain("9 nodes  ·  3 kept  ·  2 stale  ·  ⇣ following");
    act(() => tree.unmount());
  });

  it("should_ShowTheQuestionAndTheEffectsBadge_When_TheRepeatQuestionScreenIsAsked", () => {
    const tree = draw("#surface/repeat-question");
    expect(tree.root.findAll((node) => String(node.props.className ?? "").split(" ").includes("badge-effects"))[0]).toBeDefined();
    act(() => tree.unmount());
  });

  it("should_FollowTheAddressBar_When_TheHashNamesAnotherScreen", () => {
    const listeners: Record<string, () => void> = {};
    vi.stubGlobal("window", {
      location: { hash: "#surface/cells-keys/paper" },
      addEventListener: (name: string, run: () => void) => { listeners[name] = run; },
      removeEventListener: (name: string) => { delete listeners[name]; },
    });
    const tree = draw("#surface/cells-keys/paper");
    expect(tree.root.findByProps({ className: "wes-terminal surface-terminal gallery" }).props["data-palette"]).toBe("paper");
    window.location.hash = "#surface/cells-controls/ink";
    act(() => listeners.hashchange!());
    const root = tree.root.findByProps({ className: "wes-terminal surface-terminal gallery" });
    expect(root.props["data-palette"]).toBe("ink");
    expect(tree.root.findAllByType("section").filter(node => node.props["data-cell"] !== undefined)[0]!.props["data-theme"]).toBe("controls");
    act(() => tree.unmount());
  });

  it("should_NameEveryScreenTheGatesNeed_When_TheGalleriesAreListed", () => {
    expect(Object.keys(GALLERIES)).toEqual([
      "mono-lines", "cells-keys", "cells-controls",
      "values", "session", "editor", "graph", "stale", "env", "settings", "open",
      "split-2", "split-3", "split-4", "repeat-question",
    ]);
  });
});
