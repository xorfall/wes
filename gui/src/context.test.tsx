import { expect, it, vi, afterEach } from "vitest";
import { contextStatus, sameContext, type Environments } from "./context";
import { apply, emptyWorkspace } from "./workspace";

const state: Environments = { event: "environments", managed: true, revisions: { dev: "r1", prod: "r2" }, enabled: { dev: true, prod: false }, credentials: {}, providers: {}, clients: {} };
const dev = { selected: "dev", revisions: state.revisions };
const prod = { selected: "prod", revisions: state.revisions };
afterEach(() => vi.restoreAllMocks());

it("distinguishes loading, local and unselected managed state without inventing fallback", () => {
  expect(contextStatus(undefined, undefined, undefined).label).toBe("Loading…");
  expect(contextStatus({ ...state, managed: false }, undefined, undefined).label).toBe("Local");
  const unselected = contextStatus(state, { selected: null, revisions: {} }, undefined);
  expect(unselected.label).toBe("Choose environment");
  expect(unselected.unselected).toBe(true);
});

it("labels the captured draft instead of silently showing the newly selected destination", () => {
  const draft = { generation: "one", context: dev, sessionChanged: false };
  const status = contextStatus(state, prod, draft);
  expect(status.label).toBe("dev");
  expect(status.changed).toBe(true);
  expect(status.disabled).toBe(false);
  expect(contextStatus(state, prod, undefined).disabled).toBe(true);
});

it("exposes missing bindings, revision changes and old sessions independently", () => {
  const draft = { generation: "one", context: dev, sessionChanged: true };
  expect(contextStatus({ ...state, revisions: { prod: "r2" } }, prod, draft).missing).toBe(true);
  const status = contextStatus({ ...state, revisions: { dev: "r3" } }, dev, draft);
  expect(status.stale).toBe(true);
  expect(status.sessionChanged).toBe(true);
  expect(sameContext(dev, { selected: "dev", revisions: { prod: "r2", dev: "r1" } })).toBe(true);
  expect(sameContext(undefined, { selected: null, revisions: {} })).toBe(false);
});

it("clears workspace identity on session replacement and projection failure", () => {
  const event = { event: "workspace-context", name: "research", saved: ["research", "demo"] } as const;
  const workspace = apply(emptyWorkspace, event);
  expect(workspace.identity).toEqual(event);
  expect(apply(workspace, { event: "session", generation: "new" }).identity).toBeUndefined();
  expect(apply(workspace, { event: "projection-unavailable", message: "unavailable" }).identity).toBeUndefined();
});
