import type { Environments } from "../context";
import type { AuthenticationReport, ProviderAuthentication } from "../environment-authentication";
import type { ProviderWire } from "../protocol";
import type { Environment, EnvironmentProvider } from "./screens/Env";

/** Hex characters of a revision shown on screen; the whole revision stays the identity. */
const SHORT_REVISION = 8;
/** Chips a collapsed card draws per kind before the rest is left to `+N more`. */
export const CHIPS_PER_KIND = 8;
/** What a fact the service did not send reads as: never a guess. */
export const NOT_REPORTED = "not reported";

/** How a reported source kind reads on screen; kinds without an entry read as sent. */
const KIND_LABELS: Readonly<Record<string, string>> = { spec: "API", builtin: "built-in" };

/**
 * Where a provider runs and what it contacts, from the service's resolved import metadata.
 * Older services may omit these fields; the UI must never infer a target from a provider name.
 */
interface ProviderPlacement {
  readonly kind?: string;
  readonly target?: string;
  readonly endpoint?: string | null;
}

/** A revision as a person compares it: its first hex characters, without the algorithm prefix. */
export function shortRevision(revision: string): string {
  return revision.replace(/^sha256:/, "").slice(0, SHORT_REVISION);
}

function readProvider(wire: ProviderWire & ProviderPlacement): EnvironmentProvider {
  return {
    name: wire.name,
    ...(wire.kind ? { kind: wire.kind } : {}),
    ...(wire.target ? { target: wire.target } : {}),
    ...(wire.endpoint ? { endpoint: wire.endpoint } : {}),
    writes: wire.capabilities.some(capability => !capability.safe),
    credentials: (wire.credentials ?? []).map(({ name, supplied }) => ({ name, supplied })),
  };
}

/** Supplied credentials out of those declared, or nothing when no provider declares any. */
function countCredentials(providers: readonly EnvironmentProvider[]): Environment["credentials"] {
  const wanted = providers.flatMap(provider => provider.credentials);
  return wanted.length ? { present: wanted.filter(credential => credential.supplied).length, wanted: wanted.length } : undefined;
}

function withCount(environment: Environment, providers: readonly EnvironmentProvider[]): Environment {
  const { credentials: _, ...rest } = environment;
  const credentials = countCredentials(providers);
  return { ...rest, providers, ...(credentials ? { credentials } : {}) };
}

/**
 * The environments event, read for the screen: each environment's providers, how many of the
 * credentials they need are supplied, whether it may run and whether it can change real systems.
 */
export function readEnvironments(state: Environments | undefined): Environment[] {
  if (!state) return [];
  return Object.entries(state.revisions).map(([name, revision]) => {
    const providers = (state.providers[name] ?? []).map(readProvider);
    return withCount({
      name, revision, providers,
      execution: state.enabled[name] === true,
      dangerous: providers.some(provider => provider.writes),
    }, providers);
  });
}

/**
 * The environment as the authentication report knows it. Where the report covers a provider, its
 * slots and their presence are authoritative and its setup travels with the provider; providers the
 * event does not list yet are added. Nothing else about the environment changes.
 */
export function withAuthentication(environment: Environment, report: AuthenticationReport | undefined): Environment {
  const covered = (report?.providers ?? []).filter(entry => entry.environment === environment.name);
  if (covered.length === 0) return environment;
  const merged = environment.providers.map(provider => {
    const authentication = covered.find(entry => entry.provider === provider.name);
    return authentication ? authenticated(provider, authentication) : provider;
  });
  for (const authentication of covered) {
    if (!merged.some(provider => provider.name === authentication.provider)) {
      merged.push(authenticated({ name: authentication.provider, writes: false, credentials: [] }, authentication));
    }
  }
  return withCount(environment, merged);
}

function authenticated(provider: EnvironmentProvider, authentication: ProviderAuthentication): EnvironmentProvider {
  return { ...provider, authentication, credentials: authentication.credentials.map(slot => ({ name: slot.slot, supplied: slot.present })) };
}

/** The kind as a person reads it, or nothing when the service did not say. */
export function kindLabel(provider: EnvironmentProvider): string | undefined {
  return provider.kind ? KIND_LABELS[provider.kind] ?? provider.kind : undefined;
}

/** Where it runs and, apart from that, what it contacts: `target → endpoint`. */
export function runsOn(provider: EnvironmentProvider): string {
  if (!provider.target && !provider.endpoint) return NOT_REPORTED;
  return [provider.target ?? NOT_REPORTED, ...(provider.endpoint ? [provider.endpoint] : [])].join(" → ");
}

/** The execution targets the environment's providers reported, each once, in order. */
export function targetsOf(environment: Environment): string[] {
  return [...new Set(environment.providers.flatMap(provider => provider.target ? [provider.target] : []))];
}

export interface ChipGroup {
  /** The kind, or nothing for providers whose kind was not reported. */
  readonly kind?: string;
  readonly providers: readonly EnvironmentProvider[];
  /** How many more of this kind the collapsed card leaves to its open state. */
  readonly hidden: number;
}

/** Providers grouped by kind in the order kinds first appear, each group bounded. */
export function chipGroups(providers: readonly EnvironmentProvider[], limit = CHIPS_PER_KIND): ChipGroup[] {
  const groups = new Map<string | undefined, EnvironmentProvider[]>();
  for (const provider of providers) {
    const kind = kindLabel(provider);
    groups.set(kind, [...(groups.get(kind) ?? []), provider]);
  }
  return [...groups].map(([kind, members]) => ({
    ...(kind ? { kind } : {}),
    providers: members.slice(0, limit),
    hidden: Math.max(0, members.length - limit),
  }));
}

/** Whether any credential the provider declares is not supplied. */
export function missingCredentials(provider: EnvironmentProvider): boolean {
  return provider.credentials.some(credential => !credential.supplied);
}

const optionKey = (schemes: readonly string[]) => JSON.stringify([...schemes].sort());

/** A method choice as the service stores it: the option's schemes, sorted. */
export function methodChoice(schemes: readonly string[]): string[] {
  return [...schemes].sort();
}

/** Whether two method choices name the same option. */
export function sameMethod(a: readonly string[] | null | undefined, b: readonly string[]): boolean {
  return a !== null && a !== undefined && optionKey(a) === optionKey(b);
}

/** The saved method choices of the operations that have a choice. */
export function savedChoices(provider: ProviderAuthentication): Record<string, string[]> {
  return Object.fromEntries(provider.operations
    .filter(operation => operation.state !== "fixed" && operation.selected !== null)
    .map(operation => [operation.operation.join(" "), operation.selected!]));
}

/**
 * What a provider's setup stands on for the given choices: the credential slots the chosen methods
 * need, whether any is not bound yet, whether an operation is left undecided, and whether every
 * bound slot is supplied. The service's guards are the authority; these only order the steps.
 */
export function setupFacts(provider: ProviderAuthentication, choices: Record<string, string[]> = savedChoices(provider)) {
  const required = Array.from(new Set(provider.operations.flatMap(operation => operation.state === "fixed"
    ? operation.options[0]?.credentialSlots ?? []
    : operation.options.find(option => sameMethod(choices[operation.operation.join(" ")], option.schemes))?.credentialSlots ?? [])));
  const slots = provider.credentials;
  return {
    required,
    unbound: required.some(slot => !slots.some(credential => credential.slot === slot)),
    unresolved: provider.operations.some(operation => operation.state === "selection-required"),
    supplied: slots.every(credential => credential.present),
  };
}

/** Whether the saved setup still wants something before access can be granted. */
export function needsSetup(provider: ProviderAuthentication): boolean {
  const facts = setupFacts(provider);
  return facts.unresolved || facts.unbound || (facts.required.length > 0 && !facts.supplied);
}
