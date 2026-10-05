import { describe, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { lineText, MonoLine } from "../MonoLine";
import { leaving } from "../Screen";
import { EnvScreen, confirmation, envHeader, envNote, statusOf } from "./Env";
import { OpenScreen, tabKeys, type OpenTab } from "./Open";
import type { StoredValue } from "../../protocol";
import { SettingsScreen, tabAfter } from "./Settings";
import { GraphScreen, cycleLine, graphSubject, selectionFacts } from "./Graph";
import { laidOut, NODE_MAX_WIDTH, NODE_WIDTH, widthOf } from "./GraphCanvas";
import * as fixtures from "./fixtures";
import { topLine } from "../session-model";

const top = topLine({ workspace: "sales-api", environment: "DEV", connection: "connected" });

function textOf(node: { children: readonly unknown[] }): string {
  return node.children
    .map((child) => (typeof child === "string" ? child : textOf(child as { children: readonly unknown[] })))
    .join("");
}
const lines = (tree: ReactTestRenderer) => tree.root.findAllByType("pre").map(textOf);
const chips = (tree: ReactTestRenderer) => tree.root.findAllByType("button").map((b) => textOf(b));

function draw(element: React.ReactElement): ReactTestRenderer {
  let tree: ReactTestRenderer | undefined;
  act(() => { tree = create(element); });
  return tree!;
}

const escape = (tree: ReactTestRenderer) =>
  act(() =>
    tree.root.findByType("section").props.onKeyDown({ key: "Escape", preventDefault() {}, stopPropagation() {} }),
  );

describe("every screen's chrome", () => {
  it("should_SayHowToLeaveFirst_When_TheFooterIsWritten", () => {
    expect(lineText(leaving())).toBe("esc back to the session, the half-typed line intact");
    expect(lineText(leaving({ text: "p", role: "mono-ref" }, { text: " open in a pane instead", role: "mono-dim" })))
      .toBe("esc back to the session, the half-typed line intact   p open in a pane instead");
  });

  it("should_ReturnToTheSession_When_EscapeIsPressed", () => {
    const onClose = vi.fn();
    const tree = draw(<OpenScreen top={top} subject={fixtures.openSubject} tab="result" onClose={onClose} />);
    escape(tree);
    expect(onClose).toHaveBeenCalledOnce();
    act(() => tree.unmount());
  });
});

describe("/env", () => {
  const drawEnv = (props = {}) =>
    draw(<EnvScreen top={top} environments={fixtures.environments} chosen="DEV" {...props} />);

  it("should_ReadEachCardsFactsAndOneState_When_TheScreenIsDrawn", () => {
    const tree = drawEnv();
    expect(lines(tree)).toEqual([
      "wes  /  sales-api  ·  connected",
      "4 environments · DEV selected",
      "DEV",
      "STAGING",
      "PROD",
      "Local",
      "esc back to the session, the half-typed line intact",
    ]);
    expect(tree.root.findAll((node) => node.props.className === "env-facts").map((p) => textOf(p))).toEqual([
      "runs on local · rev 14aa0000",
      "runs on local · rev 14bb0000",
      "runs on local · rev 14cc0000",
      "no providers",
    ]);
    expect(textOf(tree.root.findByProps({ className: "screen-label env-note" }))).toBe(envNote());
    act(() => tree.unmount());
  });

  it("should_SayExecutionOffBeforeCredentials_When_AnEnvironmentMayNotRun", () => {
    expect(statusOf({ name: "x", providers: [], credentials: { present: 0, wanted: 2 }, execution: false }).map(s => s.text)).toEqual(["disabled", "credentials 0/2"]);
    expect(statusOf(fixtures.environments[1]!).find((s) => s.text.startsWith("credentials"))!.role).toBe("mono-warn");
    expect(lineText(envHeader(fixtures.environments.slice(0, 1)))).toBe("1 environment");
  });

  it("should_DressCardsInRolesAndMarkTheOneThatWritesReal_When_TheCardsAreDrawn", () => {
    const tree = drawEnv();
    const cards = tree.root.findAllByType("article");
    expect(cards.map((card) => card.props.className)).toEqual([
      "env-card option-chosen", "env-card option", "env-card option env-card-dangerous", "env-card option",
    ]);
    expect(tree.root.findAll((node) => String(node.props.className ?? "").includes("rail-failed"))).toHaveLength(1);
    act(() => tree.unmount());
  });

  it("should_ChooseAtOnce_When_TheEnvironmentIsNotDangerous", () => {
    const onChoose = vi.fn();
    const tree = drawEnv({ onChoose });
    act(() => tree.root.findAllByProps({ role: "radio" })[1]!.props.onClick());
    expect(onChoose).toHaveBeenCalledWith("STAGING");
    act(() => tree.unmount());
  });

  it("should_AskInsideItsCard_When_TheEnvironmentWritesReal", () => {
    const onChoose = vi.fn();
    const tree = drawEnv({ onChoose });
    act(() => tree.root.findAllByProps({ role: "radio" })[2]!.props.onClick());
    expect(onChoose).not.toHaveBeenCalled();
    const question = tree.root.findByProps({ role: "alert" });
    expect(textOf(question)).toContain(confirmation("PROD"));
    expect(tree.root.findAllByType("article")[2]!.findAllByProps({ role: "alert" })).toHaveLength(1);
    act(() => question.findAllByType("button")[0]!.props.onClick());
    expect(onChoose).toHaveBeenCalledWith("PROD");
    act(() => tree.unmount());
  });

  it("presents status and credential counts as explanatory noninteractive badges", () => {
    const tree = drawEnv();
    const card = tree.root.findAllByType("article")[2]!;
    const badges = card.findAll(node => typeof node.type === "string" && String(node.props.className ?? "").startsWith("env-badge "));
    expect(badges.map(textOf)).toEqual(["enabled", "writes", "credentials 2/2"]);
    for (const badge of badges) {
      expect(badge.type).toBe("span");
      expect(badge.props.onClick).toBeUndefined();
      expect(badge.props.tabIndex).toBeUndefined();
      expect(badge.props["aria-label"]).toBe(badge.props["aria-description"]);
    }
    expect(badges[0]!.props["aria-description"]).toContain("Execution enabled");
    expect(badges[1]!.props["aria-description"]).toContain("change real systems");
    expect(badges[2]!.props["aria-description"]).toContain("does not grant access");
    act(() => tree.unmount());
  });

  it.each(["DEV", "PROD"])("never reselects %s via mouse, Enter or Space", chosen => {
    const onChoose = vi.fn();
    const tree = drawEnv({ chosen, onChoose });
    const row = tree.root.findAllByProps({ role: "radio" }).find(row => row.props["aria-checked"])!;
    for (let attempt = 0; attempt < 2; attempt++) {
      act(() => row.props.onClick());
      for (const key of ["Enter", " "]) act(() => row.props.onKeyDown({ key, preventDefault() {}, stopPropagation() {} }));
    }
    expect(tree.root.findAllByProps({ role: "alert" })).toHaveLength(0);
    expect(onChoose).not.toHaveBeenCalled();
    act(() => tree.unmount());
  });

  it("dismisses a pending confirmation when the selected environment changes", () => {
    const onChoose = vi.fn();
    const tree = drawEnv({ onChoose });
    act(() => tree.root.findAllByProps({ role: "radio" })[2]!.props.onClick());
    expect(tree.root.findAllByProps({ role: "alert" })).toHaveLength(1);
    act(() => tree.update(<EnvScreen top={top} environments={fixtures.environments} chosen="PROD" onChoose={onChoose} />));
    expect(tree.root.findAllByProps({ role: "alert" })).toHaveLength(0);
    act(() => tree.root.findAllByProps({ role: "radio" })[2]!.props.onKeyDown({ key: "Enter", preventDefault() {}, stopPropagation() {} }));
    expect(onChoose).not.toHaveBeenCalled();
    act(() => tree.update(<EnvScreen top={top} environments={fixtures.environments} chosen="DEV" onChoose={onChoose} />));
    expect(tree.root.findAllByProps({ role: "alert" })).toHaveLength(0);
    act(() => tree.unmount());
  });

  it("should_TakeBackTheQuestionFirst_When_EscapeIsPressedWhileAsking", () => {
    const onClose = vi.fn();
    const onChoose = vi.fn();
    const tree = drawEnv({ onChoose, onClose });
    act(() => tree.root.findAllByProps({ role: "radio" })[2]!.props.onClick());
    escape(tree);
    expect(onClose).not.toHaveBeenCalled();
    expect(onChoose).not.toHaveBeenCalled();
    escape(tree);
    expect(onClose).toHaveBeenCalledOnce();
    act(() => tree.unmount());
  });

  it("should_KeepTheSetupInsideTheChosenCard_When_AnAuthenticationBindingIsGiven", () => {
    vi.stubGlobal("fetch", vi.fn(() => new Promise(() => undefined)));
    const onChoose = vi.fn();
    const tree = drawEnv({ onChoose, authentication: { workspace: "w", generation: "g" } });
    const setup = tree.root.findByProps({ "aria-label": "Authentication" });
    // The setup sits in the chosen card, beside (not inside) its radio line, so a click there never re-chooses.
    expect(tree.root.findAllByType("article")[0]!.findAllByProps({ "aria-label": "Authentication" })).toHaveLength(1);
    expect(tree.root.findAllByProps({ role: "radio" })[0]!.findAllByProps({ "aria-label": "Authentication" })).toHaveLength(0);
    expect(setup).toBeDefined();
    expect(onChoose).not.toHaveBeenCalled();
    act(() => tree.unmount());
    vi.unstubAllGlobals();
  });

  const disclosure = (tree: ReactTestRenderer, name: string) =>
    tree.root.findAll((node) => node.type === "button" && node.props.className === "env-disclosure" && String(node.props["aria-label"]).endsWith(` ${name}`))[0]!;
  const tables = (tree: ReactTestRenderer) => tree.root.findAllByProps({ role: "table" }).length;

  it("opens the chosen card by itself and any other without choosing it", () => {
    const onChoose = vi.fn();
    const tree = drawEnv({ onChoose });
    expect(tree.root.findAll((node) => node.props.className === "env-disclosure").map((node) => node.props["aria-expanded"])).toEqual([true, false, false, false]);
    expect(tables(tree)).toBe(1);
    act(() => disclosure(tree, "PROD").props.onClick());
    expect(disclosure(tree, "PROD").props["aria-expanded"]).toBe(true);
    expect(tables(tree)).toBe(2);
    expect(onChoose).not.toHaveBeenCalled();
    expect(tree.root.findAllByProps({ role: "alert" })).toHaveLength(0);
    // The disclosure is beside the radio line, never inside it.
    for (const radio of tree.root.findAllByProps({ role: "radio" })) expect(radio.findAll((node) => node.props.className === "env-disclosure")).toHaveLength(0);
    act(() => disclosure(tree, "DEV").props.onClick());
    expect(disclosure(tree, "DEV").props["aria-expanded"]).toBe(false);
    act(() => tree.root.findAllByType("button").find((b) => textOf(b) === "collapse all")!.props.onClick());
    expect(tables(tree)).toBe(0);
    act(() => tree.root.findAllByType("button").find((b) => textOf(b) === "expand all")!.props.onClick());
    expect(tables(tree)).toBe(3);
    act(() => tree.unmount());
  });

  it("reopens a card when it becomes the chosen one", () => {
    const tree = drawEnv();
    act(() => disclosure(tree, "DEV").props.onClick());
    act(() => tree.update(<EnvScreen top={top} environments={fixtures.environments} chosen="STAGING" />));
    expect(disclosure(tree, "STAGING").props["aria-expanded"]).toBe(true);
    act(() => tree.update(<EnvScreen top={top} environments={fixtures.environments} chosen="DEV" />));
    expect(disclosure(tree, "DEV").props["aria-expanded"]).toBe(true);
    act(() => tree.unmount());
  });

  it("groups a closed card's providers by kind, bounded, with +N more opening the card", () => {
    const provider = (name: string, kind?: string) => ({ name, ...(kind ? { kind } : {}), writes: false, credentials: [] });
    const many = [...Array.from({ length: 11 }, (_, at) => provider(`api${at}`, "spec")), provider("docker", "builtin"), provider("mystery")];
    const tree = drawEnv({ environments: [{ name: "wide", providers: many }], chosen: "" });
    const groups = tree.root.findAll((node) => node.props.className === "env-chip-group");
    expect(groups.map((group) => group.findAll((node) => node.props.className === "env-chip-kind").map(textOf))).toEqual([["API"], ["built-in"], []]);
    expect(groups[0]!.findAll((node) => node.props.className === "env-chip")).toHaveLength(8);
    const more = tree.root.findAllByType("button").find((b) => textOf(b) === "+3 more")!;
    act(() => more.props.onClick());
    expect(disclosure(tree, "wide").props["aria-expanded"]).toBe(true);
    expect(tree.root.findAllByProps({ role: "row" })).toHaveLength(1 + many.length);
    expect(tree.root.findAll((node) => node.props.className === "env-chip")).toHaveLength(0);
    act(() => tree.unmount());
  });

  it("tells where a provider runs apart from what it contacts, without an effects column", () => {
    const tree = drawEnv();
    const headers = tree.root.findAllByProps({ role: "columnheader" }).map(textOf);
    expect(headers).toEqual(["provider", "kind", "runs on → contacts", "credentials", "access"]);
    const cells = tree.root.findAllByProps({ role: "cell" }).map(textOf);
    expect(cells).toContain("local → https://dev.acme.test");
    expect(cells).toContain("API");
    expect(cells).toContain("built-in");
    expect(tree.root.findAll((node) => /\bStep\b|env-step/.test(String(node.props.className ?? "")))).toHaveLength(0);
    act(() => tree.unmount());
  });

  it("should_SayWhatConfirmingMeans_When_TheQuestionIsWritten", () => {
    expect(confirmation("PROD")).toBe("Select PROD? Its commands can change real systems. Every cell run under it is marked.");
  });

  it("offers one enable control on the selected disabled environment without reselecting it", () => {
    const onEnable = vi.fn(), onChoose = vi.fn();
    const environments = fixtures.environments.map(e => ({ ...e, execution: false }));
    const tree = drawEnv({ environments, onEnable, onChoose });
    const buttons = tree.root.findAllByType("button").filter(b => textOf(b) === "enable environment");
    expect(buttons).toHaveLength(1);
    expect(tree.root.findAllByType("article")[0]!.findAllByType("button")).toContain(buttons[0]);
    act(() => buttons[0]!.props.onClick());
    expect(onEnable).toHaveBeenCalledExactlyOnceWith("DEV");
    expect(onChoose).not.toHaveBeenCalled();
    act(() => tree.update(<EnvScreen top={top} environments={environments.map(e => ({ ...e, execution: true }))} chosen="DEV" onEnable={onEnable} />));
    expect(tree.root.findAllByType("button").some(b => textOf(b) === "enable environment")).toBe(false);
    act(() => tree.unmount());
  });
});

/** 128 synthetic orders: more than a window page. */
const orders: StoredValue = {
  type: { kind: "list", element: { kind: "record", name: "Order", fields: [{ name: "id", type: { kind: "primitive", name: "TEXT" } }, { name: "total", type: { kind: "primitive", name: "INT" } }] } },
  data: Array.from({ length: 128 }, (_, at) => ({ id: String(10431 + at), total: at })),
  provenance: {},
};

describe("/open", () => {
  const drawOpen = (props = {}) =>
    draw(
      <OpenScreen
        top={top}
        subject={fixtures.openSubject}
        tab="result"
        value={orders}
        details={fixtures.openDetails}
        json='{"id":"10431"}'
        {...props}
      />,
    );

  it("should_ReadTheFixtureSubjectAndTailAndNote_When_TheResultTabIsOpen", () => {
    const tree = drawOpen();
    const said = lines(tree);
    expect(said).toContain("$orders   acme orders.list  ·  09:12  ·  table 128×6  ·  kept");
    expect(said).toContain("⇥ next tab   v json   d details");
    expect(said).toContain("+78 rows · show 50 more");
    // The pager counts the rows; the counts under the value do not say it again.
    expect(said.filter((line) => line === "+78 rows")).toEqual([]);
    expect(said).toContain("esc back to the session, the half-typed line intact   p open in a pane instead");
    act(() => tree.unmount());
  });

  it("should_PageTheResultByFifty_When_TheResultTabIsOpen", () => {
    const tree = drawOpen();
    const rowsOf = () => tree.root.findAll((node) => node.props.className === "value-table" && node.type === "table")[0]!.findAllByProps({ role: "row" }).filter((node) => node.type === "tr");
    // A header and one page of fifty, against the cell's six-line preview.
    expect(rowsOf()).toHaveLength(51);
    // The table pages itself, under itself, a page at a time in the window.
    act(() => tree.root.findByProps({ className: "value-table-more" }).props.onClick());
    expect(rowsOf()).toHaveLength(101);
    act(() => tree.unmount());
  });

  it("should_NameTheThreeTabs_When_TheScreenIsDrawn", () => {
    const tree = drawOpen();
    expect(tree.root.findAllByProps({ role: "tab" }).filter(tab=>String(tab.props.className).includes("chip")).map((tab) => textOf(tab))).toEqual(["result", "json", "details"]);
    expect(chips(tree).slice(0, 3)).toEqual(["result", "json", "details"]);
    act(() => tree.unmount());
  });

  it("should_LightOnlyTheOpenTab_When_OneIsChosen", () => {
    for (const tab of ["result", "json", "details"] as OpenTab[]) {
      const tree = drawOpen({ tab });
      const chosen = tree.root.findAllByProps({ role: "tab" }).filter((it) => it.props["aria-selected"] && String(it.props.className).includes("chip"));
      expect(chosen.map((it) => textOf(it)), tab).toEqual([tab]);
      expect(String(chosen[0]!.props.className), tab).toContain("chip-chosen");
      act(() => tree.unmount());
    }
  });

  it("should_WalkTheTabs_When_TheKeysArePressed", () => {
    const onTab = vi.fn();
    const tree = drawOpen({ onTab });
    const panel = tree.root.findByProps({ role: "tabpanel" });
    act(() => panel.props.onKeyDown({ key: "Tab", preventDefault() {} }));
    expect(onTab).toHaveBeenLastCalledWith("json");
    act(() => panel.props.onKeyDown({ key: "v", preventDefault() {} }));
    expect(onTab).toHaveBeenLastCalledWith("json");
    act(() => panel.props.onKeyDown({ key: "d", preventDefault() {} }));
    expect(onTab).toHaveBeenLastCalledWith("details");
    act(() => tree.unmount());
  });

  it("should_ShowTheDetails_When_TheDetailsTabIsOpen", () => {
    const tree = drawOpen({ tab: "details" });
    // Grouped by command, run and session, each fact a `key value` row read off the node itself.
    expect(lines(tree)).toContain("about the run");
    expect(lines(tree)).toContain("environment     DEV");
    expect(lines(tree)).toContain("revision        rev 14");
    expect(lines(tree)).not.toContain("nested fields are summarized here; JSON shows the complete value");
    act(() => tree.unmount());
  });

  it("should_SayHowMuchIsOnScreen_When_TheTailIsWritten", () => {
    expect(lineText(tabKeys())).toBe("⇥ next tab   v json   d details");
  });
});

describe("/settings", () => {
  const drawSettings = (props = {}) =>
    draw(
      <SettingsScreen
        top={top}
        sections={[...fixtures.settingsSections]}
        section="appearance"
        rows={fixtures.appearanceRows}
        preview={fixtures.settingsPreview}
        {...props}
      />,
    );

  const tabs = (tree: ReactTestRenderer) => tree.root.findAllByProps({ role: "tab" }).filter((it) => typeof it.type === "string");

  it("should_OfferEverySectionAsATab_When_TheScreenIsDrawn", () => {
    // Arrange / Act
    const tree = drawSettings();
    // Assert: one tab row, the eight sections in order, the open one selected and the only one in the tab order
    expect(tree.root.findAllByProps({ role: "tablist" }).filter((it) => typeof it.type === "string")).toHaveLength(1);
    expect(tabs(tree).map(textOf)).toEqual(["appearance", "editor", "results", "keys", "connections", "aliases", "data", "limits"]);
    expect(tabs(tree).filter((tab) => tab.props["aria-selected"]).map(textOf)).toEqual(["appearance"]);
    expect(tabs(tree).filter((tab) => tab.props.tabIndex === 0).map(textOf)).toEqual(["appearance"]);
    expect(fixtures.settingsSections).toHaveLength(8);
    act(() => tree.unmount());
  });

  it("should_KeepTheTabs_When_TheScreenIsInAPane", () => {
    // A section that cannot be reached is not a section: the tabs are tools, and tools stay in a pane.
    const tree = drawSettings({ chrome: "pane" });
    expect(tabs(tree)).toHaveLength(8);
    act(() => tree.unmount());
  });

  it("should_AskForTheSection_When_ATabIsPressed", () => {
    // Arrange
    const onSection = vi.fn();
    const tree = drawSettings({ onSection });
    // Act
    act(() => tabs(tree)[3]!.props.onClick());
    act(() => tabs(tree)[0]!.props.onClick());
    // Assert: the open section is not asked for again
    expect(onSection).toHaveBeenCalledTimes(1);
    expect(onSection).toHaveBeenCalledWith("keys");
    act(() => tree.unmount());
  });

  it("should_MoveAlongTheRowAndWrap_When_ArrowKeysAreUsedOnTheTabs", () => {
    const sections = [...fixtures.settingsSections];
    expect(tabAfter(sections, "appearance", "ArrowRight")).toBe("editor");
    expect(tabAfter(sections, "appearance", "ArrowLeft")).toBe("limits");
    expect(tabAfter(sections, "limits", "ArrowRight")).toBe("appearance");
    expect(tabAfter(sections, "keys", "Home")).toBe("appearance");
    expect(tabAfter(sections, "keys", "End")).toBe("limits");
    expect(tabAfter(sections, "keys", "Enter")).toBeUndefined();
    expect(tabAfter(sections, "elsewhere", "ArrowRight")).toBeUndefined();
  });

  it("should_ShowEachRowsOwnCommand_When_TheRowsAreDrawn", () => {
    const tree = drawSettings();
    const said = lines(tree);
    expect(said).toContain("/theme paper");
    expect(said).toContain("/theme cell bordered");
    expect(said).toContain('/theme font "PT Mono" 13');
    expect(said).toContain("esc back to the session, the half-typed line intact   every row here is a command you can type");
    act(() => tree.unmount());
  });

  it("should_DrawEachFixedSetAsJoinedChipsAndSayWhatTheChosenMeans_When_TheRowIsDrawn", () => {
    const tree = drawSettings();
    const chips = tree.root.findAllByProps({ role: "radio" }).map((it) => textOf(it));
    for (const name of ["paper", "ink", "minimal", "compact"]) expect(chips, name).toContain(name);
    const meanings = tree.root.findAll((node) => node.props.className === "screen-label settings-meaning").map((it) => textOf(it));
    expect(meanings).toContain("paper · light · warm");
    expect(meanings.some((it) => it.startsWith("ink"))).toBe(false); // only the chosen option is explained
    act(() => tree.unmount());
  });

  it("should_LightTheChosenChip_When_ASettingHoldsAValue", () => {
    const tree = drawSettings();
    const radios = tree.root.findAllByProps({ role: "radio" });
    const chosen = radios.filter((it) => it.props["aria-checked"]);
    expect(chosen).toHaveLength(3);
    for (const chip of chosen) expect(String(chip.props.className)).toBe("screen-chip settings-segment chip-chosen");
    for (const chip of radios.filter((it) => !it.props["aria-checked"])) expect(String(chip.props.className)).toBe("screen-chip settings-segment cell-action");
    act(() => tree.unmount());
  });

  it("should_TellWhichOptionWasChosen_When_ACardIsPressed", () => {
    const onChoose = vi.fn();
    const tree = drawSettings({ onChoose });
    act(() => tree.root.findAllByProps({ role: "radio" })[1]!.props.onClick());
    expect(onChoose).toHaveBeenCalledWith(fixtures.appearanceRows[0], fixtures.appearanceRows[0]!.options[1]);
    act(() => tree.unmount());
  });

  it("should_ShowTheChosenFaceBeforeItIsChosen_When_ThePreviewIsDrawn", () => {
    const tree = drawSettings();
    expect(lines(tree)).toContain("PT Mono 13 / 1.62  —  acme orders.list since:2026-09-01 > orders");
    act(() => tree.unmount());
  });
});

describe("/graph", () => {
  const drawGraph = (props = {}) =>
    draw(
      <GraphScreen
        top={top}
        nodes={fixtures.graphNodes}
        edges={fixtures.graphEdges}
        cycles={fixtures.graphCycles}
        selected={fixtures.graphSelected}
        {...props}
      />,
    );

  it("should_CountTheGraph_When_TheSubjectIsWritten", () => {
    expect(lineText(graphSubject(fixtures.graphSelected, fixtures.graphNodes, fixtures.graphEdges, fixtures.graphCycles)))
      .toBe("$totals   9 nodes  ·  11 edges  ·  3 cycles");
  });

  it("should_SayNothingAboutCycles_When_ThereAreNone", () => {
    expect(lineText(graphSubject(undefined, fixtures.graphNodes, fixtures.graphEdges, [])))
      .toBe("9 nodes  ·  11 edges");
  });

  it("should_AnswerWhyItIsStaleAndWhatBreaks_When_ANodeIsSelected", () => {
    expect(selectionFacts(fixtures.graphSelected).map((line) => lineText(line))).toEqual([
      "state        stale",
      "because      $orders ran again",
      "made by      cell 09:16",
      "shape        record · 4 fields",
      "breaks       1 dependent",
    ]);
  });

  it("should_CloseEachCycle_When_TheCyclesAreWritten", () => {
    expect(fixtures.graphCycles.map((cycle) => lineText(cycleLine(cycle)))).toEqual([
      "$totals → $spread → $totals",
      "$ledger → $totals → $ledger",
      "$regions → $totals → $regions",
    ]);
  });

  it("should_OfferTheResultToolbar_When_TheScreenIsDrawn", () => {
    const tree = drawGraph();
    expect(chips(tree)).toEqual(["stale only", "hide unconnected", "layout ↔ LR", "fit", "⌕ find node"]);
    act(() => tree.unmount());
  });

  it("should_LightStaleOnly_When_TheScreenWasOpenedByStale", () => {
    const tree = drawGraph({ staleOnly: true });
    const chip = tree.root.findAllByType("button")[0]!;
    expect(chip.props["aria-pressed"]).toBe(true);
    expect(String(chip.props.className)).toContain("chip-chosen");
    act(() => tree.unmount());
  });

  it("should_OfferToHideUnconnectedNodes_And_SayHowManyWereHidden", () => {
    // Arrange
    const onConnectedOnly = vi.fn();
    const tree = drawGraph({ connectedOnly: true, hidden: 3, onConnectedOnly });
    const chip = tree.root.findAllByType("button").find((it) => it.children.join("") === "hide unconnected")!;
    // Act
    act(() => chip.props.onClick());
    // Assert
    expect(chip.props["aria-pressed"]).toBe(true);
    expect(onConnectedOnly).toHaveBeenCalledWith(false);
    const facts = tree.root.findAllByType(MonoLine).map((line) => lineText(line.props.segments));
    expect(facts.some((text) => text.includes("connected only") && text.includes("3 hidden"))).toBe(true);
    act(() => tree.unmount());
  });

  it("should_TurnTheLayout_When_TheChipIsPressed", () => {
    const onDirection = vi.fn();
    const tree = drawGraph({ onDirection });
    act(() => tree.root.findAllByType("button").find((it) => it.children.join("") === "layout ↔ LR")!.props.onClick());
    expect(onDirection).toHaveBeenCalledWith("TB");
    act(() => tree.unmount());
  });

  it("should_NameTheKeysTheScreenAnswersTo_When_ItIsDrawn", () => {
    const tree = drawGraph();
    const said = lines(tree);
    expect(said).toContain("esc close   ⏎ jump to cell");
    expect(said).toContain("⏎ jump to cell");
    expect(said).toContain("↻ repeat + 1");
    expect(said).toContain("⊙ open result");
    expect(said).toContain("esc back to the session, the half-typed line intact   ⇥ next node   / another screen");
    act(() => tree.unmount());
  });

  it("should_DoWhatEachOfferSays_When_ItIsPressed", () => {
    const onJump = vi.fn();
    const onRepeat = vi.fn();
    const onOpenResult = vi.fn();
    const tree = drawGraph({ onJump, onRepeat, onOpenResult });
    const offers = tree.root.findAllByProps({ className: "graph-panel-key cell-action" });
    expect(offers).toHaveLength(3);
    act(() => offers[0]!.props.onClick());
    act(() => offers[1]!.props.onClick());
    act(() => offers[2]!.props.onClick());
    expect(onJump).toHaveBeenCalled();
    expect(onRepeat).toHaveBeenCalled();
    expect(onOpenResult).toHaveBeenCalled();
    act(() => tree.unmount());
  });

  it("should_SayNoNodesYet_When_TheWorkspaceIsEmpty", () => {
    const tree = drawGraph({ nodes: [], edges: [], cycles: [], canvas: undefined });
    expect(lines(tree)).toContain("no nodes yet");
    expect(tree.root.findAllByProps({ className: "graph-canvas" })).toHaveLength(0);
    act(() => tree.unmount());
  });
});

describe("the graph's layout", () => {
  it("should_PlaceEveryNode_When_TheGraphIsLaidOut", () => {
    const placed = laidOut(fixtures.graphNodes, fixtures.graphEdges, "LR");
    expect(placed.size).toBe(9);
    for (const node of fixtures.graphNodes) expect(placed.has(node.id), node.id).toBe(true);
  });

  it("should_GrowANodeToHoldItsSubLine_When_TheTypeIsLongerThanTheDefault", () => {
    const narrow = widthOf({ id: "a", name: "$code", detail: "Int · 277 B", state: "ok" });
    const wide = widthOf({ id: "b", name: "$caps", detail: "List<Unknown> · 2 rows", state: "ok" });
    expect(narrow).toBe(NODE_WIDTH);
    expect(wide).toBeGreaterThan(NODE_WIDTH);
    expect(wide).toBeLessThanOrEqual(NODE_MAX_WIDTH);
  });

  it("should_StopAtTheWidestAndLetTheTextBeCut_When_TheSubLineIsLongerStill", () => {
    const enormous = widthOf({ id: "c", name: "$x", detail: "List<Record<VeryLongTypeNameIndeed>> · 1 048 576 rows", state: "ok" });
    expect(enormous).toBe(NODE_MAX_WIDTH);
  });

  it("should_PlaceTheBoxesItWillDraw_When_TheNodesAreDifferentWidths", () => {
    const placed = laidOut(fixtures.graphNodes, fixtures.graphEdges, "LR");
    for (const node of fixtures.graphNodes) {
      expect(placed.get(node.id)!.width, node.id).toBe(widthOf(node));
    }
  });

  it("should_GrowTheWayItIsAsked_When_TheDirectionChanges", () => {
    const across = laidOut(fixtures.graphNodes, fixtures.graphEdges, "LR");
    const down = laidOut(fixtures.graphNodes, fixtures.graphEdges, "TB");
    // `$acme` feeds `$orders`, so one is to the left of the other across and above it down.
    expect(across.get("acme")!.x).toBeLessThan(across.get("orders")!.x);
    expect(down.get("acme")!.y).toBeLessThan(down.get("orders")!.y);
  });
});
