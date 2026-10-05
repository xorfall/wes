/**
 * The surface's own galleries, drawn from fixtures.
 *
 * Hash routes such as `#surface/cells-keys/ink` render synthetic screens for visual regression
 * checks across palettes. The fixtures are independent of the active workspace and cannot
 * execute providers. The galleries are not linked from the workspace interface.
 */
import { useEffect, useState } from "react";
import { Cell, type CellBlock, type CellState, type Theme } from "./Cell";
import { MonoLine, type Segment } from "./MonoLine";
import { MonoLine as Line } from "./MonoLine";
import { commandSegments } from "./command-line";
import { ValueBlock } from "./render/ValueBlock";
import type { StoredValue, TypeShape } from "../protocol";
import type { Identity, SourceRow, VerdictField } from "./session-model";
import { Session } from "./Session";
import { readSession, type SessionCell } from "./session-model";
import { sessionCells, sessionContext, sessionNow, sessionWorkspace } from "./session-fixture";
import { Editor } from "./Editor";
import { editKeys } from "./edit-model";
import { bundledPackage, readLanguage } from "./language";
import { topLine, contextLine } from "./session-model";
import { EnvScreen } from "./screens/Env";
import { OpenScreen, type OpenTab } from "./screens/Open";
import { SettingsScreen } from "./screens/Settings";
import { GraphScreen } from "./screens/Graph";
import { GraphCanvas } from "./screens/GraphCanvas";
import * as screens from "./screens/fixtures";
import { Split } from "./Split";
import { open, oneP, type Pane, type SplitState } from "./split-model";
import type { Palette, Density } from "./axes";
import "./surface.css";
import "./gallery.css";

/** The command line of the Mono lines screen. */
const COMMAND: Segment[] = [
  { text: "09:14  ", role: "mono-faint" },
  { text: "❯ ", role: "mono-ref-strong" },
  { text: "acme orders.list", role: "mono-provider" },
  { text: " " },
  { text: "since:", role: "mono-param" },
  { text: "2026-09-01", role: "mono-literal" },
];

const TEXT: TypeShape = { kind: "primitive", name: "TEXT" };
const INT: TypeShape = { kind: "primitive", name: "INT" };
const DECIMAL: TypeShape = { kind: "primitive", name: "DECIMAL" };
const BYTES: TypeShape = { kind: "primitive", name: "BYTES" };
const record = (name: string, fields: Record<string, TypeShape>): TypeShape =>
  ({ kind: "record", name, fields: Object.entries(fields).map(([field, type]) => ({ name: field, type })) });
const stored = (type: TypeShape, data: unknown): StoredValue => ({ type, data, provenance: {} });
const b64 = (text: string) => btoa(String.fromCharCode(...new TextEncoder().encode(text)));

/** Synthetic values: invented customers, hosts and numbers, never a user's workspace. */
export const GALLERY_VALUES = {
  orders: stored({ kind: "list", element: record("Order", { id: TEXT, customer: TEXT, total: DECIMAL, status: TEXT }) }, [
    { id: "10431", customer: "Northwind Ltd", total: 248, status: "paid" },
    { id: "10432", customer: "Contoso GmbH", total: 1940, status: "pending" },
    { id: "10433", customer: "Acme Retail", total: 86.5, status: "paid" },
    ...Array.from({ length: 125 }, (_, at) => ({ id: String(10434 + at), customer: "Fabrikam", total: at, status: "paid" })),
  ]),
  usage: stored({ kind: "list", element: INT }, [412, 388, 301, 97, 64, 58, 12]),
  customer: stored(record("Customer", { id: TEXT, name: TEXT, region: TEXT, since: TEXT, orders: INT, note: TEXT }), {
    id: "10432", name: "Contoso GmbH", region: "eu-west", since: "2019-04-02", orders: 118, note: "prefers invoices by post\n",
  }),
  process: stored(record("ProcessOutput", { exitCode: INT, stdout: BYTES, stderr: BYTES }), {
    exitCode: 1, stdout: "", stderr: b64("cat: /srv/missing.conf: No such file or directory\n"),
  }),
  http: stored(record("HttpResponse", {
    status: INT, version: TEXT, headers: { kind: "list", element: record("HttpHeader", { name: TEXT, value: TEXT }) }, body: BYTES,
  }), {
    status: 404, version: "HTTP/1.1", headers: [{ name: "content-type", value: "application/json" }, { name: "content-length", value: "31" }],
    body: b64('{"ok":false,"path":"/missing"}'),
  }),
  deps: stored({ kind: "unknown" }, { nodes: [{ id: "orders" }, { id: "totals" }, { id: "daily" }], edges: [{ from: "orders", to: "totals" }, { from: "totals", to: "daily" }] }),
} as const;

/** Source rows for a one-line command made by one node. */
function rows(text: string, identity?: Identity): SourceRow[] {
  return [{ segments: commandSegments(text), nodes: identity ? [identity] : [] }];
}

const field = (text: string, role: Segment["role"], keep = false, slot?: VerdictField["slot"]): VerdictField => ({ segments: [{ text, role }], keep, slot });
const verdict = (...fields: VerdictField[]): VerdictField[] => fields.map((field,at)=>at===0?{...field,slot:"state"}:field);

function valueBlock(key: string, identity: Identity, value: StoredValue): CellBlock {
  return { key, identity, row: 0, open: true, content: <ValueBlock value={value} cacheKey={`gallery:${key}`} mode="preview" /> };
}

/** One verdict per state, in the grammar: state · shape · facts · time · retention. */
const VERDICTS: Record<CellState, VerdictField[]> = {
  default: verdict(field("ok", "mono-ok", true), field("List<Order>", "mono-ink", true), field("128", "mono-dim"), field("340 ms", "mono-dim", false, "duration"), field("kept", "mono-dim", false, "retention")),
  focus: verdict(field("ok", "mono-ok", true), field("List<Order>", "mono-ink", true), field("128", "mono-dim"), field("340 ms", "mono-dim", false, "duration"), field("kept", "mono-dim", false, "retention")),
  stale: verdict(field("ok", "mono-ok", true), field("List<Order>", "mono-ink", true), field("128", "mono-dim"), field("1 stale", "mono-warn", false, "retention")),
  pinned: verdict(field("ok", "mono-ok", true), field("List<Order>", "mono-ink", true), field("128", "mono-dim"), field("pinned", "mono-ref", false, "retention")),
  failed: verdict(field("failed", "mono-bad-strong", true), field("UPSTREAM_503", "mono-ink", true), field("12 ms", "mono-dim", false, "duration"), field("not kept", "mono-dim", false, "retention")),
  live: verdict(field("running", "mono-meta-strong", true), field("List<Event>", "mono-ink", true), field("18 s", "mono-dim", false, "duration"), field("● live", "mono-meta", false, "retention")),
};

/** Every action a gallery cell offers, so each state's strip shows what that state is worth. */
const ACTIONS = {
  repeat: () => {}, branch: () => {}, pin: () => {}, open: () => {}, json: () => {},
  details: () => {}, edit: () => {}, cycle: () => {}, cancel: () => {}, follow: () => {},
  copy: () => {}, view: () => {},
};

const STATES: CellState[] = ["default", "focus", "stale", "failed", "pinned", "live"];

const GLYPHS: Record<CellState, Identity["glyph"]> = { default: "ready", focus: "ready", stale: "stale", pinned: "ready", failed: "failed", live: "running" };

function CellsGallery({ theme }: { readonly theme: Theme }) {
  return (
    <div className="gallery-grid">
      {STATES.map((state) => {
        const identity: Identity = { id: "n7", label: "$orders", glyph: GLYPHS[state] };
        const block: CellBlock = state === "failed"
          ? { key: "n7", identity, row: 0, open: true, content: <Line segments={[{ text: "UPSTREAM_503", role: "mono-bad" }, { text: " · ", role: "mono-faint" }, { text: "the provider answered 503", role: "mono-bad" }]} /> }
          : state === "live"
            ? { key: "n7", identity, row: 0, open: true, content: <Line segments={[{ text: "● running", role: "mono-meta" }, { text: " · the result appears when the command finishes or stops", role: "mono-faint" }]} /> }
            : valueBlock(`orders-${theme}-${state}`, identity, GALLERY_VALUES.orders);
        return (
          <Cell key={state} theme={theme} state={state} rows={rows("acme orders.list since:2026-09-01 > orders", identity)} time="09:14"
            chars={42} verdict={VERDICTS[state]} actions={ACTIONS} label={`${theme} ${state}`} blocks={[block]} />
        );
      })}
    </div>
  );
}

/** The three lines of the surface's "Mono lines" screen: a command, its verdict, its diagnostic. */
function MonoLines() {
  const command: Segment[] = [
    ...COMMAND.slice(0, 6),
    { text: " " },
    { text: "statis:", role: "mono-param" },
    { text: '"open"', role: "mono-literal" },
  ];
  return (
    <div className="gallery-scrollback">
      <MonoLine segments={command} />
      <MonoLine
        segments={[
          { text: "failed", role: "mono-bad-strong" },
          { text: " · ", role: "mono-faint" },
          { text: "unknown parameter", role: "mono-ink" },
          { text: " · ", role: "mono-faint" },
          { text: "12 ms", role: "mono-dim" },
          { text: " · ", role: "mono-faint" },
          { text: "not kept", role: "mono-dim" },
        ]}
      />
      <MonoLine
        segments={[
          { text: " ".repeat(43) },
          { text: "^^^^^^^", role: "mono-bad" },
          { text: " no such parameter; the package offers status:", role: "mono-bad" },
        ]}
      />
    </div>
  );
}

/** Values of every kind, each presented by the one pipeline the cells use. */
const VALUES: readonly { key: keyof typeof GALLERY_VALUES; command: string; shape: string; facts?: VerdictField }[] = [
  { key: "orders", command: "acme orders.list since:2026-09-01 > orders", shape: "List<Order>", facts: field("128", "mono-dim") },
  { key: "usage", command: "acme usage.by_region > usage", shape: "List<Int>", facts: field("7", "mono-dim") },
  { key: "customer", command: "acme customer.get id:10432 > customer", shape: "Customer" },
  { key: "process", command: 'sh run cmd:"cat /srv/missing.conf" > conf', shape: "ProcessOutput", facts: field("exit 1", "mono-warn") },
  { key: "http", command: 'http request url:"http://127.0.0.1:8080/missing" > probe', shape: "HttpResponse", facts: field("404", "mono-warn") },
  { key: "deps", command: ":calc { return $graph; } > deps", shape: "record" },
];

function ValuesGallery() {
  return (
    <div className="gallery-grid">
      {VALUES.map(({ key, command, shape, facts }) => {
        const identity: Identity = { id: key, label: `$${key}`, glyph: "ready" };
        return (
          <Cell key={key} theme="keys" state="default" rows={rows(command, identity)} time="09:14" chars={command.length}
            verdict={verdict(field("ok", "mono-ok", true), field(shape, "mono-ink", true), ...(facts ? [facts] : []), field("kept", "mono-dim", false, "retention"))}
            actions={ACTIONS} label={`${key} value`} blocks={[valueBlock(`values-${key}`, identity, GALLERY_VALUES[key])]} />
        );
      })}
    </div>
  );
}

/**
 * The guarded-repeat question, asked, so the gate can see the row.
 *
 * Full width, as a cell in the session is: the row is one line, and one line of it is 107
 * characters, which a 660-wide gallery column cuts before the keys that answer it.
 */
function RepeatQuestionGallery() {
  const identity: Identity = { id: "refund", label: "$refund", glyph: "ready" };
  return (
    <div className="gallery-full">
      <Cell theme="keys" state="default" rows={rows("acme refunds.create id:10432 > refund", identity)} time="09:14" chars={38}
        verdict={verdict(field("ok", "mono-ok", true), field("Customer", "mono-ink", true), field("412 ms", "mono-dim"), field("kept", "mono-dim", false, "retention"))}
        marks={[{ kind: "effects", text: "effects acknowledged" }]}
        actions={ACTIONS}
        confirmRepeat={{ what: "acme refunds.create", against: "PROD", dependents: ["$totals", "$daily"] }}
        blocks={[valueBlock("repeat-customer", identity, GALLERY_VALUES.customer)]}
        label="a guarded repeat, asking" />
    </div>
  );
}

/**
 * The session screen, from the synthetic workspace: the projection is the running client's, and
 * each block is drawn by the same presentation pipeline from synthetic values.
 */
function sessionBlocks(cell: SessionCell): readonly CellBlock[] {
  const node = cell.nodes[0];
  if (!node) return [];
  const values: Record<string, StoredValue> = { orders: GALLERY_VALUES.orders, usage: GALLERY_VALUES.usage, deps: GALLERY_VALUES.deps };
  if (values[node.id]) return [valueBlock(`session-${node.id}`, node, values[node.id]!)];
  if (node.glyph === "running") return [{ key: node.id, identity: node, row: 0, open: true, content: <Line segments={[{ text: "● running", role: "mono-meta" }, { text: " · the result appears when the command finishes or stops", role: "mono-faint" }]} /> }];
  if (node.failure) return [{ key: node.id, identity: node, row: 0, open: true, content: <Line segments={[{ text: node.failure.code ?? "failed", role: "mono-bad" }, { text: " · ", role: "mono-faint" }, { text: node.failure.message, role: "mono-bad" }]} /> }];
  return [];
}

function SessionGallery() {
  const model = readSession(
    { workspace: sessionWorkspace, cells: sessionCells, context: sessionContext },
    sessionNow,
  );
  return (
    <Session
      model={model}
      chrome="keys"
      prompt={
        <Line
          segments={[
            { text: "❯ ", role: "mono-ref-strong" },
            { text: "acme invoices.list ", role: "mono-provider" },
            { text: "since:", role: "mono-param" },
            { text: "2026-09-15", role: "mono-literal" },
            { text: "█", role: "mono-ref" },
          ]}
          className="session-prompt-line"
        />
      }
      actions={() => ACTIONS}
      output={sessionBlocks}
    />
  );
}

/** The surface's editor screen: a compact cell above, the program, and the completion list. */
const PROGRAM = [
  ":calc {",
  '  const paid = $orders.filter(o => o.status == "paid");',
  "  if (paid !== none) return paid.count();",
  "  return paid.reduce((t, o) => t + o.total, 0.0);",
  "} > revenue",
].join("\n");

function EditorGallery() {
  const language = readLanguage(bundledPackage, "engine");
  return (
    <div className="gallery-screen">
      <div className="gallery-screen-top surface-sunk">
        <Line segments={[...topLine({ ...sessionContext, connection: "connected" }).slice(0, 5), { text: "  ·  ", role: "mono-faint" }, { text: "editing", role: "mono-meta" }]} />
      </div>
      <div className="gallery-screen-body">
        <Cell theme="keys" state="default" view="collapsed" rows={rows("acme orders.list since:2026-09-01 > orders", { id: "orders", label: "$orders", glyph: "ready" })}
          time="09:12" chars={42} verdict={verdict(field("ok", "mono-ok", true), field("List<Order>", "mono-ink", true), field("kept", "mono-dim", false, "retention"))}
          label="previous command" />
      </div>
      {/* The drawing only: the field that types into it is the prompt's own, and the prompt is
          not on this page. What the gallery shows is the gutter, the lines and the mistake rows. */}
      <Editor source={PROGRAM} language={language} />
      <div className="gallery-screen-foot">
        <Line segments={contextLine(sessionContext)} />
        <Line segments={editKeys()} />
      </div>
    </div>
  );
}

/** The session's own top line, which every screen keeps so it never loses where it is. */
const TOP = topLine(sessionContext);

function EnvGallery() {
  const [chosen, setChosen] = useState("DEV");
  return <EnvScreen top={TOP} environments={screens.environments} chosen={chosen} onChoose={setChosen} />;
}

function OpenGallery() {
  const [tab, setTab] = useState<OpenTab>("result");
  return (
    <OpenScreen
      top={TOP}
      subject={screens.openSubject}
      tab={tab}
      onTab={setTab}
      value={GALLERY_VALUES.orders}
      details={screens.openDetails}
      json={'{\n  "id": "10431",\n  "customer": "Northwind Ltd"\n}'}
    />
  );
}

function SettingsGallery() {
  return (
    <SettingsScreen
      top={TOP}
      sections={[...screens.settingsSections]}
      section="appearance"
      rows={screens.appearanceRows}
      preview={screens.settingsPreview}
    />
  );
}

/**
 * `/graph`, and `/graph` as `/stale` opens it.
 *
 * `/stale` opens the dependency graph with the stale-only filter enabled.
 */
function GraphGallery({ staleOnly = false }: { readonly staleOnly?: boolean }) {
  const [only, setOnly] = useState(staleOnly);
  const [direction, setDirection] = useState<"LR" | "TB">("LR");
  const shown = only
    ? screens.graphNodes.filter((node) => node.state === "stale" || node.state === "selected")
    : screens.graphNodes;
  const kept = new Set(shown.map((node) => node.id));
  const edges = screens.graphEdges.filter((edge) => kept.has(edge.from) && kept.has(edge.to));
  return (
    <GraphScreen
      top={TOP}
      nodes={shown}
      edges={edges}
      cycles={screens.graphCycles}
      selected={screens.graphSelected}
      staleOnly={only}
      direction={direction}
      onStaleOnly={setOnly}
      onDirection={setDirection}
      canvas={<GraphCanvas nodes={shown} edges={edges} direction={direction} />}
    />
  );
}

/** The two compact cells the split captures show in their session pane. */
const PANE_CELLS = (
  <>
    <Cell theme="keys" state="default" view="collapsed" rows={rows("acme usage.by_region > usage", { id: "usage", label: "$usage", glyph: "ready" })} time="09:14" chars={28}
      verdict={verdict(field("ok", "mono-ok", true), field("List<Int>", "mono-ink", true), field("7", "mono-dim"), field("8 ms", "mono-dim"))} label="session usage" />
    <Cell theme="keys" state="live" view="collapsed" rows={rows("acme events.tail > events", { id: "events", label: "$events", glyph: "running" })} time="09:14" chars={25}
      verdict={verdict(field("running", "mono-meta-strong", true), field("running", "mono-ink", true), field("18 s", "mono-dim", false, "duration"))} label="session live" />
  </>
);

const SESSION_PANE: Pane = { id: "p1", title: "session" };
const GRAPH_PANE: Pane = { id: "p2", title: "/graph $orders", command: "/graph $orders" };
const OPEN_PANE: Pane = { id: "p3", title: "/open acme orders.list", command: "/open acme orders.list" };
const ENV_PANE: Pane = { id: "p4", title: "/env DEV", command: "/env DEV" };

/**
 * What each pane actually holds.
 *
 * Any surface in any pane, so a `/graph` pane holds the graph and an `/open` pane
 * holds the result — not a note saying it would. Each screen drops its own chrome in a pane: the
 * pane's head says what it is and the split's footer says how to leave.
 */
function paneContent(pane: Pane): JSX.Element {
  if (pane.id === SESSION_PANE.id) return PANE_CELLS;
  if (pane.id === GRAPH_PANE.id) {
    return (
      <GraphScreen
        chrome="pane"
        top={TOP}
        nodes={screens.graphNodes}
        edges={screens.graphEdges}
        cycles={screens.graphCycles}
        selected={screens.graphSelected}
        canvas={<GraphCanvas nodes={screens.graphNodes} edges={screens.graphEdges} direction="LR" />}
      />
    );
  }
  if (pane.id === OPEN_PANE.id) {
    return (
      <OpenScreen
        chrome="pane"
        top={TOP}
        subject={screens.openSubject}
        tab="result"
        value={GALLERY_VALUES.orders}
      />
    );
  }
  return <EnvScreen chrome="pane" top={TOP} environments={screens.environments} chosen="DEV" />;
}

function SplitGallery({ panes }: { readonly panes: 2 | 3 | 4 }) {
  const [state, setState] = useState<SplitState>(() => {
    let built = open(oneP(SESSION_PANE), GRAPH_PANE);
    if (panes >= 3) built = open(built, OPEN_PANE);
    if (panes >= 4) built = open(built, ENV_PANE);
    // The caret is in the session in every split capture, whatever opened last.
    return { ...built, focused: SESSION_PANE.id };
  });
  return (
    <Split
      state={state}
      onChange={setState}
      top={TOP}
      prompt={[
        { text: "❯ ", role: "mono-ref-strong" },
        { text: "acme invoices.list ", role: "mono-provider" },
        { text: "since:", role: "mono-param" },
        { text: "2026-09-15", role: "mono-literal" },
        { text: "█", role: "mono-ref" },
      ]}
      context={contextLine(sessionContext)}
      content={(pane: Pane) => paneContent(pane)}
    />
  );
}

export const GALLERIES: Record<string, () => JSX.Element> = {
  "mono-lines": () => <MonoLines />,
  "cells-keys": () => <CellsGallery theme="keys" />,
  "cells-controls": () => <CellsGallery theme="controls" />,
  values: () => <ValuesGallery />,
  session: () => <SessionGallery />,
  editor: () => <EditorGallery />,
  graph: () => <GraphGallery />,
  stale: () => <GraphGallery staleOnly />,
  env: () => <EnvGallery />,
  settings: () => <SettingsGallery />,
  open: () => <OpenGallery />,
  // A key per arrangement, so walking from one to the next builds the arrangement asked for rather
  // than keeping the state the last one left behind.
  "split-2": () => <SplitGallery key="split-2" panes={2} />,
  "split-3": () => <SplitGallery key="split-3" panes={3} />,
  "split-4": () => <SplitGallery key="split-4" panes={4} />,
  "repeat-question": () => <RepeatQuestionGallery />,
};

export interface GalleryRoute {
  readonly screen: string;
  readonly palette: Palette;
  readonly density: Density;
  /** Draw at the fixed 1440×900 reference size, scaled to fit whatever window is available. */
  readonly framed: boolean;
}

/** Fixed gallery dimensions for repeatable screenshot comparisons. */
export const FRAME = { width: 1440, height: 900 } as const;

/** `#surface/cells-keys/ink/dense` — screen, then whichever axis values were named. */
export function readGalleryRoute(hash: string): GalleryRoute | undefined {
  const parts = hash.replace(/^#/, "").split("/").filter(Boolean);
  if (parts[0] !== "surface") return undefined;
  const rest = parts.slice(1);
  return {
    screen: rest.find((part) => part in GALLERIES) ?? "cells-keys",
    palette: rest.includes("ink") ? "ink" : rest.includes("white") ? "white" : "paper",
    density: rest.includes("dense") ? "dense" : "normal",
    framed: rest.includes("1440x900"),
  };
}

/**
 * How much the 1440×900 reference frame has to shrink to fit the window it is being looked at in.
 *
 * A screen compared at a different size is a different screen: a scrollback that fits five cells at
 * 900 and four at 797 would fail a comparison it should pass. So the frame stays 1440×900 and the
 * picture of it gets smaller.
 */
export function frameScale(width: number, height: number): number {
  return Math.min(1, width / FRAME.width, height / FRAME.height);
}

/**
 * Follows the address bar, so one tab can walk every screen in both palettes.
 *
 * A gate is eight pictures of the same build; reloading between each would be eight builds, and a
 * hash that changed the address without changing the page would be worse — it would look like the
 * screen had been taken and it would be the previous one.
 */
export function useGalleryRoute(initial: GalleryRoute): GalleryRoute {
  const [route, setRoute] = useState(initial);
  useEffect(() => {
    // Rendered outside a browser — a test tree — there is no address bar to follow.
    if (typeof window === "undefined") return;
    const follow = () => setRoute(readGalleryRoute(window.location.hash) ?? initial);
    window.addEventListener("hashchange", follow);
    return () => window.removeEventListener("hashchange", follow);
    // The initial route is read once, at mount; afterwards the address bar is the only authority.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return route;
}

export function Gallery({ route: initial }: { readonly route: GalleryRoute }) {
  const route = useGalleryRoute(initial);
  const draw = GALLERIES[route.screen] ?? GALLERIES["cells-keys"]!;
  const [scale, setScale] = useState(1);
  useEffect(() => {
    if (!route.framed || typeof window === "undefined") return setScale(1);
    const fit = () => setScale(frameScale(window.innerWidth, window.innerHeight));
    fit();
    window.addEventListener("resize", fit);
    return () => window.removeEventListener("resize", fit);
  }, [route.framed]);
  return (
    <div
      className={`wes-terminal surface-terminal gallery${route.framed ? " gallery-framed" : ""}`}
      data-palette={route.palette}
      data-density={route.density}
      style={route.framed ? { width: FRAME.width, height: FRAME.height, zoom: scale } : undefined}
    >
      {draw()}
    </div>
  );
}
