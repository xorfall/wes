import { expect, it } from "vitest";
import type { Environments } from "../context";
import type { AuthenticationReport } from "../environment-authentication";
import { chipGroups, needsSetup, NOT_REPORTED, readEnvironments, runsOn, shortRevision, targetsOf, withAuthentication } from "./environment-model";

const capability = (safe: boolean) => ({ path: ["x"], summary: "", result: "", safe, parameters: [] });
const event = (): Environments => ({
  event: "environments", managed: true, default: "keypair", clients: {}, credentials: {},
  enabled: { keypair: false, local: true },
  revisions: { keypair: "sha256:a7813d9ed680b0aa8cfb98961383b7c5e3072d3764bdd11e33759ff15ed08a23", local: "sha256:c7cf42d9f51d90c2" },
  providers: {
    keypair: [{ name: "demo", ready: false, capabilities: [capability(true)], credentials: [{ name: "apiKey", supplied: true }, { name: "apiSecret", supplied: false }] }],
    local: [{ name: "sh", ready: true, capabilities: [capability(false)], credentials: [] }],
  },
});

it("should_CountSuppliedCredentialsAndKeepTheWholeRevision_When_TheEventIsRead", () => {
  // Act
  const [keypair, local] = readEnvironments(event());
  // Assert
  expect(keypair).toEqual({ name: "keypair", credentials: { present: 1, wanted: 2 }, execution: false, dangerous: false,
    providers: [{ name: "demo", writes: false, credentials: [{ name: "apiKey", supplied: true }, { name: "apiSecret", supplied: false }] }],
    revision: "sha256:a7813d9ed680b0aa8cfb98961383b7c5e3072d3764bdd11e33759ff15ed08a23" });
  expect(local).toMatchObject({ name: "local", execution: true, dangerous: true, providers: [{ name: "sh", writes: true }] });
  expect(local!.credentials).toBeUndefined();
  expect(shortRevision(keypair!.revision!)).toBe("a7813d9e");
});

it("should_ReadPlacementOnlyWhenTheServiceSendsIt_When_ProvidersAreRead", () => {
  // Arrange
  const state = event();
  const placed = { ...state.providers.keypair![0]!, kind: "spec", target: "local", endpoint: "http://127.0.0.1:8765" };
  // Act
  const [keypair, local] = readEnvironments({ ...state, providers: { ...state.providers, keypair: [placed] } });
  // Assert
  expect(keypair!.providers[0]).toMatchObject({ kind: "spec", target: "local", endpoint: "http://127.0.0.1:8765" });
  expect(runsOn(keypair!.providers[0]!)).toBe("local → http://127.0.0.1:8765");
  expect(runsOn(local!.providers[0]!)).toBe(NOT_REPORTED);
  expect(runsOn({ name: "x", endpoint: "https://api.test", writes: false, credentials: [] })).toBe(`${NOT_REPORTED} → https://api.test`);
  expect(targetsOf(keypair!)).toEqual(["local"]);
  expect(targetsOf(local!)).toEqual([]);
});

it("should_PreferTheReportsPresenceAndAddItsProviders_When_AuthenticationIsKnown", () => {
  // Arrange
  const [keypair] = readEnvironments(event());
  const report: AuthenticationReport = { workspace: "w", generation: "g", providers: [
    { environment: "keypair", revision: "r", provider: "demo", enabled: false, grantSeconds: 0, operations: [], credentials: [{ slot: "apiKey", reference: "a", present: true }, { slot: "apiSecret", reference: "b", present: true }] },
    { environment: "keypair", revision: "r", provider: "extra", enabled: false, grantSeconds: 0, operations: [{ operation: ["get"], state: "selection-required", selected: null, options: [] }], credentials: [] },
    { environment: "other", revision: "r", provider: "demo", enabled: false, grantSeconds: 0, operations: [], credentials: [{ slot: "k", reference: "k", present: false }] },
  ] };
  // Act
  const merged = withAuthentication(keypair!, report);
  // Assert
  expect(merged.credentials).toEqual({ present: 2, wanted: 2 });
  expect(merged.providers.map(provider => provider.name)).toEqual(["demo", "extra"]);
  expect(merged.providers[0]!.authentication?.provider).toBe("demo");
  expect(needsSetup(merged.providers[1]!.authentication!)).toBe(true);
  expect(withAuthentication(keypair!, undefined)).toBe(keypair);
});

it("should_GroupByKindInFirstSeenOrderAndBoundEachGroup_When_ChipsAreDrawn", () => {
  // Arrange
  const provider = (name: string, kind?: string) => ({ name, ...(kind ? { kind } : {}), writes: false, credentials: [] });
  const providers = [provider("sh", "builtin"), ...Array.from({ length: 10 }, (_, at) => provider(`api${at}`, "spec")), provider("tool", "process"), provider("bare")];
  // Act
  const groups = chipGroups(providers);
  // Assert
  expect(groups.map(group => [group.kind, group.providers.length, group.hidden])).toEqual([["built-in", 1, 0], ["API", 8, 2], ["process", 1, 0], [undefined, 1, 0]]);
});
