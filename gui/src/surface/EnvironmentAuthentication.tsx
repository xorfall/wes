/**
 * An open environment card's providers: one table (provider, kind, runs on → contacts, credentials,
 * access), where a provider with authentication choices or credential bindings opens its own setup,
 * so the list and the setup never repeat each other.
 *
 * HTTP setup has three steps: Method (which authentication each operation uses), Credentials (the
 * values, masked, kept in this session unless remembered on this device) and Access (this provider
 * may use the credentials for the lifetime chosen before granting, or without a time limit for this
 * live session). Fixed declarative bindings use Credentials and Access.
 *
 * The Access step separates current authority from a pending duration choice and scoped read
 * proofs. Probes stay native provider commands; this step only shows drafts, never runs them.
 * Where it runs is table metadata, not a step, and
 * execution belongs to the environment card. A step's actions wait until the steps before it are
 * done; the guards themselves are the service's and are unchanged: save before credentials, grant
 * only when bound, resolved, enabled and supplied.
 *
 * The authentication report is read once per screen by `useEnvironmentAuthentication`, so every
 * card counts the same presence and a change made in one card is seen by all.
 *
 * Where remembered credentials live in the data folder's vault, the vault is read only once a
 * provider can remember credentials. Remembering into a missing or locked vault asks for its
 * password in the same form, and a locked vault can be unlocked from the Credentials step.
 */
import { useEffect, useRef, useState, type ReactNode } from "react";
import { authenticationRequest, type AccessLifetime, type AccessLifetimes, type ReadProof, type AuthenticationReport, type AuthWorkspace, type AuthAction, type ProviderAuthentication } from "../environment-authentication";
import { MIN_PASSWORD_CHARS, useCredentialVault, vaultNeeds, type VaultControl } from "../credential-vault";
import type { Segment } from "./MonoLine";
import { TableGrid, type GridColumn } from "./render/TableGrid";
import type { Environment, EnvironmentProvider } from "./screens/Env";
import { kindLabel, methodChoice, missingCredentials, needsSetup, NOT_REPORTED, runsOn, sameMethod, savedChoices, setupFacts } from "./environment-model";
import "./render/presentation.css";
import "./authentication.css";

/** What an access grant's countdown is redrawn by. */
const TICK_MS = 1000;
const NONE = "—";
const COLUMNS: readonly GridColumn[] = [
  { name: "provider", key: true }, { name: "kind" }, { name: "runs on → contacts" }, { name: "credentials" }, { name: "access" },
];

export interface AuthenticationNotice {
  readonly text: string;
  readonly failed: boolean;
  /** The environment whose action it answers; a failed read belongs to every card. */
  readonly environment?: string;
}

export interface AuthenticationState {
  readonly report?: AuthenticationReport;
  /** When the report was read, on the `performance` clock grants count down against. */
  readonly updated: number;
  readonly busy: boolean;
  readonly notice?: AuthenticationNotice;
  readonly change: (action: AuthAction) => Promise<void>;
  readonly refresh: (environment: string) => Promise<void>;
}

/**
 * The workspace's authentication report, read when the binding or `reload` changes (an environment's
 * revision or execution), and the actions that change it. Without a binding nothing is read.
 */
export function useEnvironmentAuthentication(binding: AuthWorkspace | undefined, reload: string): AuthenticationState {
  const [report, setReport] = useState<AuthenticationReport>();
  const [notice, setNotice] = useState<AuthenticationNotice>();
  const [busy, setBusy] = useState(false);
  const [updated, setUpdated] = useState(performance.now());
  const workspace = binding?.workspace, generation = binding?.generation;
  const contextKey = JSON.stringify([workspace, generation]);
  const currentKey = useRef(contextKey); currentKey.current = contextKey;
  const reads = useRef(0);
  const load = async (signal?: AbortSignal) => {
    const read = ++reads.current;
    const result = await authenticationRequest(binding!, undefined, signal) as AuthenticationReport;
    if (signal?.aborted || currentKey.current !== contextKey || read !== reads.current) return;
    setReport(result); setUpdated(performance.now());
  };
  // Another workspace's report is never drawn under this one, not even while the next is read.
  useEffect(() => { setReport(undefined); setNotice(undefined); setBusy(false); }, [workspace, generation]);
  useEffect(() => {
    if (!binding) return;
    const abort = new AbortController();
    void load(abort.signal).catch(e => { if (!abort.signal.aborted) setNotice({ text: (e as Error).message, failed: true }); });
    return () => abort.abort();
  }, [workspace, generation, reload]);
  const change = async (action: AuthAction) => {
    if (busy || !binding) return;
    setBusy(true); setNotice(undefined);
    try {
      await authenticationRequest(binding, action);
      if (currentKey.current !== contextKey) return;
      setNotice({ environment: action.environment, failed: false, text: changedText(action) });
      await load();
    } catch (e) { if (currentKey.current === contextKey) setNotice({ environment: action.environment, failed: true, text: (e as Error).message }); }
    finally { if (currentKey.current === contextKey) setBusy(false); }
  };
  const refresh = async (environment: string) => {
    if (!binding) return;
    setBusy(true); setNotice(undefined);
    try { await load(); } catch (e) { if (currentKey.current === contextKey) setNotice({ environment, failed: true, text: (e as Error).message }); } finally { if (currentKey.current === contextKey) setBusy(false); }
  };
  return { ...(report ? { report } : {}), updated, busy, ...(notice ? { notice } : {}), change, refresh };
}

/** Seconds of access left, counted down from the report and never above what it said. */
function useRemaining(updated: number): (grantSeconds: number) => number {
  const [now, setNow] = useState(performance.now());
  useEffect(() => { const timer = setInterval(() => setNow(performance.now()), TICK_MS); return () => clearInterval(timer); }, []);
  // A refresh may arrive after the last tick. Negative elapsed time must never invent authority
  // or extend the server's reported grant duration.
  const elapsed = Math.max(0, Math.floor((now - updated) / TICK_MS));
  return grantSeconds => Math.max(0, grantSeconds - elapsed);
}

/** Missing capability metadata cannot expose custom or session access. */
const MINIMAL_LIFETIMES: AccessLifetimes = { presets: [300], minSeconds: 300, maxSeconds: 300, session: false };
/** The lifetime a fresh setup suggests, where advertised. Never `session`: that is always picked by hand. */
const RECOMMENDED_SECONDS = 900;

/** `5 minutes`, `1 hour`, `1 hour 30 minutes`; `short` for the chips: `5 min`, `1 h`. */
export function lifetimeText(seconds: number, short = false): string {
  const hours = Math.floor(seconds / 3600), minutes = Math.round((seconds % 3600) / 60);
  const part = (n: number, unit: "hour" | "minute") => short ? `${n} ${unit === "hour" ? "h" : "min"}` : `${n} ${unit}${n === 1 ? "" : "s"}`;
  return [...(hours ? [part(hours, "hour")] : []), ...(minutes || !hours ? [part(minutes, "minute")] : [])].join(" ");
}

/** A countdown as a clock: `0:45`, `14:05`, `3:59:12`. */
export function clock(seconds: number): string {
  const h = Math.floor(seconds / 3600), m = Math.floor((seconds % 3600) / 60), s = seconds % 60;
  const two = (n: number) => String(n).padStart(2, "0");
  return h ? `${h}:${two(m)}:${two(s)}` : `${m}:${two(s)}`;
}

/** The provider's access now, from the report and the local countdown; a timed grant that reached zero has expired. */
type AccessNow =
  | { readonly kind: "timed"; readonly remaining: number; readonly of: number }
  | { readonly kind: "session" }
  | { readonly kind: "none"; readonly ended?: "expired" | "revoked" | "disabled" | "changed" };

function accessNow(provider: ProviderAuthentication, seconds: number): AccessNow {
  const access = provider.access;
  // Missing capability metadata must never expose additional permission choices.
  if (!access) return seconds > 0 ? { kind: "timed", remaining: seconds, of: 300 } : { kind: "none" };
  if (access.state === "session") return { kind: "session" };
  if (access.state === "timed") return seconds > 0 ? { kind: "timed", remaining: seconds, of: access.seconds } : { kind: "none", ended: "expired" };
  return access.ended ? { kind: "none", ended: access.ended } : { kind: "none" };
}

const ENDED: Readonly<Record<NonNullable<Extract<AccessNow, { kind: "none" }>["ended"]>, string>> = {
  expired: "expired", revoked: "revoked", disabled: "ended · environment disabled", changed: "ended · credentials changed",
};

/** What the grant button and the notice call a lifetime. */
function lifetimePhrase(lifetime: AccessLifetime): string {
  return lifetime === "session" ? "until session ends" : `for ${lifetimeText(lifetime.seconds)}`;
}

function changedText(action: AuthAction): string {
  switch (action.action) {
    case "configure": return "Authentication saved. Existing cells and drafts keep their captured environment.";
    case "grant": return `Access granted to ${action.provider} ${lifetimePhrase(action.lifetime)}. Nothing was run; read scopes need a fresh native probe.`;
    case "revoke": return `Access revoked for ${action.provider}. Its read proofs ended with it.`;
    case "supply": case "forget": return "Credential updated for every provider bound to it. Their access and read proofs ended.";
    default: return "Authentication updated.";
  }
}

/** Captured public scope labels, not a provider-wide authority claim. */
function scopeText(scope: Record<string, string>): string {
  return Object.entries(scope).map(([key, value]) => `${key} ${value}`).join(" · ");
}

/** What one read proof stands on now. A proof never outlives the grant it was proven under. */
function proofStatus(state: ReadProof["state"], now: AccessNow): Segment {
  if (now.kind === "none") return state === "unverified" && now.ended !== "expired" ? { text: "needs access first", role: "mono-faint" } : { text: "ended with access", role: "mono-warn" };
  if (state === "verified") return { text: `verified · ends with this access${now.kind === "timed" ? ` in ${clock(now.remaining)}` : ""}`, role: "mono-ok" };
  return { text: "not verified · needs a fresh native probe", role: "mono-warn" };
}

function credentialsCell(provider: EnvironmentProvider): Segment[] {
  const authentication = provider.authentication;
  if (authentication && provider.credentials.length === 0) {
    const facts = setupFacts(authentication);
    if (facts.required.length) return [{ text: `${facts.required.length} needed · save the method`, role: "mono-warn" }];
    if (facts.unresolved) return [{ text: "choose a method", role: "mono-warn" }];
  }
  if (provider.credentials.length === 0) return [{ text: "none needed", role: "mono-dim" }];
  const present = provider.credentials.filter(credential => credential.supplied).length;
  const names = provider.credentials.map(credential => credential.name).join(", ");
  return missingCredentials(provider)
    ? [{ text: `● ${present}/${provider.credentials.length} missing`, role: "mono-warn" }, { text: ` · ${names}`, role: "mono-dim" }]
    : [{ text: `${present}/${provider.credentials.length}`, role: "mono-ok" }, { text: ` · ${names}`, role: "mono-dim" }];
}

function accessCell(provider: EnvironmentProvider, remaining: (grantSeconds: number) => number): Segment[] {
  const authentication = provider.authentication;
  if (!authentication || (setupFacts(authentication).required.length === 0 && !authentication.readDomain)) return [{ text: NONE, role: "mono-faint" }];
  const now = accessNow(authentication, remaining(authentication.grantSeconds));
  if (now.kind === "timed") return [{ text: `granted · ${clock(now.remaining)}`, role: "mono-ok" }, { text: ` of ${lifetimeText(now.of, true)}`, role: "mono-dim" }];
  if (now.kind === "session") return [{ text: "granted · no time limit", role: "mono-ok" }];
  return [now.ended === "expired" ? { text: "expired", role: "mono-warn" } : { text: now.ended ? ENDED[now.ended] : "not granted", role: "mono-faint" }];
}

function providerRow(provider: EnvironmentProvider, remaining: (grantSeconds: number) => number): Segment[][] {
  const kind = kindLabel(provider);
  const placed = runsOn(provider);
  return [
    [{ text: provider.name }],
    [{ text: kind ?? NOT_REPORTED, role: kind ? "mono-ink" : "mono-faint" }],
    [{ text: placed, role: placed === NOT_REPORTED ? "mono-faint" : "mono-ink" }],
    credentialsCell(provider),
    accessCell(provider, remaining),
  ];
}

/** The open card's providers and, in each authenticated provider's own row, its setup. */
export function EnvironmentProviders({ environment, state }: { environment: Environment; state?: AuthenticationState }) {
  const [open, setOpen] = useState<ReadonlySet<string>>(new Set());
  const remaining = useRemaining(state?.updated ?? 0);
  const providers = environment.providers;
  const authenticated = providers.flatMap(provider => provider.authentication ? [provider.authentication] : []);
  const pending = authenticated.filter(needsSetup).length;
  const writing = providers.filter(provider => provider.writes).length;
  const notice = state?.notice && (state.notice.environment === undefined || state.notice.environment === environment.name) ? state.notice : undefined;
  const toggle = (name: string) => setOpen(was => { const next = new Set(was); if (!next.delete(name)) next.add(name); return next; });
  const vault = useCredentialVault(authenticated.some(provider => provider.persistenceSupported));
  // Providers bound to the same credential reference still hold their own access.
  const sharing = (provider: ProviderAuthentication) => authenticated
    .filter(other => other.provider !== provider.provider && other.credentials.some(slot => provider.credentials.some(mine => mine.reference === slot.reference)))
    .map(other => other.provider);

  return <section className="env-setup" aria-label="Authentication">
    <header className="env-setup-heading">
      <p className="env-setup-title">
        <span>Providers</span>
        <span className="mono-faint">{` · ${providers.length}`}</span>
        {pending > 0 && <span className="mono-warn">{` · ${pending} need setup`}</span>}
        {writing > 0 && <span className="mono-bad">{` · ${writing} can change real systems`}</span>}
      </p>
      {state && <button type="button" className="screen-chip cell-action" aria-label="Refresh setup" aria-description="refresh" disabled={state.busy} onClick={() => void state.refresh(environment.name)}>↻</button>}
    </header>
    {state && !state.report && !notice && <p className="screen-label">Loading setup…</p>}
    {providers.length === 0
      ? <p className="screen-label">No providers in this environment.</p>
      : <TableGrid
        columns={COLUMNS}
        rows={providers.map(provider => providerRow(provider, remaining))}
        renderCell={(row, column, summary) => {
          const provider = providers[row]!;
          if (column !== 0) return summary;
          if (!provider.authentication || !state) return <><span className="env-row-toggle" aria-hidden="true" />{summary}</>;
          const shown = open.has(provider.name);
          return <><button type="button" className="env-row-toggle" aria-expanded={shown} aria-label={`${shown ? "Hide" : "Show"} ${provider.name} setup`} onClick={() => toggle(provider.name)}>{shown ? "▾" : "▸"}</button>{summary}</>;
        }}
        details={providers.map(provider => provider.authentication && state && open.has(provider.name)
          ? <ProviderSetup key={`${provider.authentication.environment}/${provider.authentication.provider}/${provider.authentication.revision}`} provider={provider.authentication} sharing={sharing(provider.authentication)} seconds={remaining(provider.authentication.grantSeconds)} busy={state.busy} onChange={state.change}
            vault={vault} onVaultChanged={() => state.refresh(environment.name)} />
          : null)}
      />}
    {notice && <p role="status" className={`env-auth-message ${notice.failed ? "mono-bad" : "mono-dim"}`}>{notice.text}</p>}
    {state && <p className="screen-label">Keys are session-only unless you choose Remember on this device. Saved keys use this computer's secure store, or this data folder's password-protected credential vault where there is none; access grants are never restored. Forget removes the value from this session and this device, keeping its environment binding. This form never calls the API.</p>}
  </section>;
}

type StepState = "done" | "current" | "waiting";

function Step({ number, title, state, children }: { number: number; title: string; state: StepState; children: ReactNode }) {
  const mark = state === "done" ? ["✓", "mono-ok"] : ["○", state === "current" ? "mono-warn" : "mono-faint"];
  return <div className={`env-step env-step-${state}`} aria-label={`${number} ${title}, ${state}`}>
    <span className={mark[1]} aria-hidden="true">{mark[0]}</span>
    <span className="screen-label env-step-title">{`${number} ${title}`}</span>
    <div className="env-step-body">{children}</div>
  </div>;
}

function ProviderSetup({ provider, sharing, seconds, busy, onChange, vault, onVaultChanged }: { provider: ProviderAuthentication; sharing: readonly string[]; seconds: number; busy: boolean; onChange: (action: AuthAction) => Promise<void>; vault: VaultControl; onVaultChanged: () => Promise<void> }) {
  const [choices, setChoices] = useState<Record<string, string[]>>(() => savedChoices(provider));
  const dirty = JSON.stringify(choices) !== JSON.stringify(savedChoices(provider));
  const { required, unbound, unresolved, supplied } = setupFacts(provider, choices);
  const slots = provider.credentials;
  const hasMethods = provider.operations.length > 0;
  const selection = { environment: provider.environment, revision: provider.revision, provider: provider.provider };
  const persistence = provider.persistenceSupported ?? false;
  const needs = persistence ? vaultNeeds(vault.report) : undefined;
  const openVault = async (password: string) => {
    if (!needs) return true;
    const opened = await vault.act({ action: needs, password });
    if (opened) await onVaultChanged();
    return opened;
  };
  const choose = (operation: string, schemes: readonly string[]) => setChoices(was => {
    const next = { ...was };
    // Choosing the chosen option again takes the unsaved choice back, as the select's empty entry did.
    if (sameMethod(was[operation], schemes)) delete next[operation]; else next[operation] = methodChoice(schemes);
    return next;
  });

  // Each step is done by the service's own facts; the next one waits for it. A saved, bound method
  // opens the credentials even while another operation is left undecided; only access needs all.
  const saved = !dirty && !unbound;
  const method = saved && !unresolved;
  // What credentials are needed is known once some method needs them or every operation is decided;
  // an empty list before anything is chosen is not "all supplied".
  const known = required.length > 0 || !unresolved;
  const credentials = saved && known && supplied;
  const execution = provider.enabled;
  const now = accessNow(provider, seconds);
  const access = now.kind !== "none";
  const state = (done: boolean, ready: boolean): StepState => done ? "done" : ready ? "current" : "waiting";

  return <div className="env-provider" aria-label={`${provider.provider} authentication`}>
    {hasMethods && <Step number={1} title="Method" state={state(method, true)}>
      {provider.operations.map(op => {
        const name = op.operation.join(" ");
        return <div className="env-auth-method" key={name}>
          <span className="mono-ink">{name}</span>
          {op.state === "fixed"
            ? <span className="mono-dim">{op.options[0]?.credentialSlots.length ? `fixed · ${op.options[0].methods.join(" + ")}` : "no credentials · fixed"}</span>
            : <div className="env-method-options" role="group" aria-label={`${provider.provider} ${name} authentication method`}>
              {op.options.map(option => {
                const pressed = sameMethod(choices[name], option.schemes);
                return <button type="button" key={JSON.stringify(methodChoice(option.schemes))} className={`env-method-option${pressed ? " on" : ""}`} aria-pressed={pressed} disabled={busy}
                  aria-description={pressed ? "Choose again to clear this unsaved choice" : undefined} onClick={() => choose(name, option.schemes)}>
                  <span className="env-method-name"><span className="env-method-dot" aria-hidden="true" />{option.schemes.length ? option.schemes.join(" + ") : "Anonymous"}</span>
                  <span className="env-method-detail">{option.methods.length ? option.methods.join(" + ") : "no credentials"}</span>
                </button>;
              })}
            </div>}
        </div>;
      })}
      <div className="env-step-actions">
        <button type="button" className="screen-chip chip-chosen" disabled={busy || (!dirty && !unbound)} onClick={() => void onChange({ ...selection, action: "configure", auth: choices })}>save authentication</button>
        {dirty ? <span className="mono-warn">unsaved choice</span> : unresolved ? <span className="mono-warn">choose a method for each operation you use</span> : unbound ? <span className="mono-warn">save to create credential bindings</span> : <span className="mono-ok">saved</span>}
      </div>
    </Step>}
    <Step number={hasMethods ? 2 : 1} title="Credentials" state={state(credentials, saved && known)}>
      {needs === "unlock" && slots.length > 0 && <VaultUnlock busy={busy || vault.busy} unlock={openVault} />}
      {vault.message?.failed && <span className="mono-bad" role="status">{vault.message.text}</span>}
      {slots.length === 0
        ? <span className="mono-dim">{required.length ? "saved method creates them" : "none needed"}</span>
        : slots.map(credential => <CredentialInput key={credential.slot} credential={credential} persistenceSupported={persistence} disabled={busy || vault.busy || !saved} vaultNeeds={needs} openVault={openVault}
          save={(value, remember) => onChange({ ...selection, action: "supply", slot: credential.slot, value, remember })}
          forget={() => onChange({ ...selection, action: "forget", slot: credential.slot })} />)}
    </Step>
    {(required.length > 0 || provider.readDomain) && <Step number={hasMethods ? 3 : 2} title="Access" state={state(access, credentials && execution)}>
      <AccessControls provider={provider} sharing={sharing} now={now} busy={busy}
        ready={!dirty && !unbound && !unresolved && execution && supplied} onChange={action => onChange({ ...selection, ...action } as AuthAction)} />
      {!execution && <span className="screen-label">Enable execution on the environment card before granting access.</span>}
    </Step>}
  </div>;
}

/** The lifetime the next grant will ask for. Choosing one changes nothing until it is granted. */
type Choice = { readonly kind: "preset"; readonly seconds: number } | { readonly kind: "custom" } | { readonly kind: "session" };
type Unit = "minutes" | "hours";

/** A custom lifetime as typed: seconds when valid, or why not. */
function customSeconds(amount: string, unit: Unit, limits: AccessLifetimes): { seconds: number } | { problem: string } {
  if (!/^\d+$/.test(amount.trim())) return { problem: "Enter a whole number." };
  const seconds = Number(amount.trim()) * (unit === "hours" ? 3600 : 60);
  if (seconds < limits.minSeconds) return { problem: `At least ${lifetimeText(limits.minSeconds)}.` };
  if (seconds > limits.maxSeconds) return { problem: `At most ${lifetimeText(limits.maxSeconds)}.` };
  return { seconds };
}

/**
 * The Access step: what is granted now and which captured read scopes stand on it, then the lifetime
 * the next grant asks for and the one button that grants it. The two never share a line, so editing
 * the next lifetime can never look like it moved the current deadline. Revoke sits beside what it ends.
 */
function AccessControls({ provider, sharing, now, busy, ready, onChange }: {
  provider: ProviderAuthentication; sharing: readonly string[]; now: AccessNow; busy: boolean; ready: boolean;
  onChange: (action: { action: "grant"; lifetime: AccessLifetime } | { action: "revoke" }) => Promise<void>;
}) {
  // Only advertised durations are selectable; every request carries its intended lifetime.
  const advertised = provider.lifetimes;
  const limits = advertised ?? MINIMAL_LIFETIMES;
  const [choice, setChoice] = useState<Choice>(() => ({ kind: "preset", seconds: limits.presets.includes(RECOMMENDED_SECONDS) ? RECOMMENDED_SECONDS : limits.presets[0] ?? limits.minSeconds }));
  const [amount, setAmount] = useState("90");
  const [unit, setUnit] = useState<Unit>("minutes");
  const [drafts, setDrafts] = useState<ReadonlySet<number>>(new Set());
  const custom = choice.kind === "custom" ? customSeconds(amount, unit, limits) : undefined;
  const lifetime: AccessLifetime | undefined = choice.kind === "session" ? "session" : choice.kind === "preset" ? { seconds: choice.seconds } : custom && "seconds" in custom ? { seconds: custom.seconds } : undefined;
  const active = now.kind !== "none";
  const proofs = provider.readScopes ?? [];
  const lapsed = now.kind === "none" && (now.ended === "expired" || proofs.some(proof => proof.state !== "unverified"));
  const unproven = active && proofs.some(proof => proof.state !== "verified");
  const toggleDraft = (at: number) => setDrafts(was => { const next = new Set(was); if (!next.delete(at)) next.add(at); return next; });
  const grant = () => { if (lifetime) void onChange({ action: "grant", lifetime }); };
  // Renewing replaces the deadline from now; say so when that is shorter than what is left.
  const left = now.kind === "session" ? "access until session ends" : now.kind === "timed" ? `the ${clock(now.remaining)} left` : undefined;
  const shorter = left && lifetime && lifetime !== "session" && (now.kind === "session" || (now.kind === "timed" && lifetime.seconds < now.remaining))
    ? `Renewing replaces ${left} with ${lifetimeText(lifetime.seconds)} from now.` : undefined;
  const options: readonly { key: string; label: string; choice: Choice }[] = [
    ...limits.presets.map(seconds => ({ key: `p${seconds}`, label: lifetimeText(seconds, true), choice: { kind: "preset", seconds } as Choice })),
    ...(limits.minSeconds < limits.maxSeconds ? [{ key: "custom", label: "custom", choice: { kind: "custom" } as Choice }] : []),
    ...(limits.session ? [{ key: "session", label: "unlimited", choice: { kind: "session" } as Choice }] : []),
  ];
  const chosen = (option: Choice) => option.kind === choice.kind && (option.kind !== "preset" || (choice.kind === "preset" && option.seconds === choice.seconds));

  return <div className="env-access">
    <span className="screen-label env-access-key">current</span>
    <div className="env-access-value env-step-actions">
      {now.kind === "timed" && <span className="mono-ok" role="timer" aria-live="off">{`granted · ${clock(now.remaining)} remaining`}<span className="mono-dim">{` of ${lifetimeText(now.of)}`}</span></span>}
      {now.kind === "session" && <span className="mono-ok">granted · until session ends</span>}
      {now.kind === "none" && <span className={now.ended === "expired" ? "mono-warn" : "mono-faint"}>{now.ended ? ENDED[now.ended] : "not granted"}</span>}
      <button type="button" className="screen-chip cell-action" disabled={busy || !active} onClick={() => void onChange({ action: "revoke" })}>revoke access</button>
    </div>
    {proofs.length === 0 && provider.readDomain && <>
      <span className="screen-label env-access-key">read scopes</span>
      <span className="screen-label">No captured scope yet. Run {provider.provider} {provider.readDomain.revalidate.join(" ")} with {provider.readDomain.scope.join(", ")} in the native prompt. Granting runs no probe.</span>
    </>}
    {proofs.length > 0 && <>
      <span className="screen-label env-access-key">read scopes</span>
      <div className="env-access-value env-access-proofs">
        {proofs.map((proof, at) => {
          const status = proofStatus(proof.state, now);
          const shown = drafts.has(at) && proof.draft !== undefined && active && proof.state !== "verified";
          return <div key={at} className="env-access-proof">
            <div className="env-step-actions">
              <span className="mono-ink">{scopeText(proof.scope)}</span>
              <span className={status.role}>{status.text}</span>
              {proof.draft !== undefined && active && proof.state !== "verified" && <button type="button" className="screen-chip cell-action" aria-expanded={shown} onClick={() => toggleDraft(at)}>{shown ? "hide command" : "prepare verification command"}</button>}
            </div>
            {shown && <div className="env-step-actions">
              <code className="env-access-draft" aria-label={`Verification command draft for ${scopeText(proof.scope)}`}>{proof.draft}</code>
              <span className="screen-label">Draft only. Nothing runs from here.</span>
            </div>}
          </div>;
        })}
        {lapsed && <span className="screen-label">Grant again, then run a fresh native probe for each scope. Reopening a View alone does not restore read access.</span>}
        {unproven && <span className="screen-label">Each scope is proven only by its own native probe. Granting or renewing runs none.</span>}
      </div>
    </>}
    {!!provider.readScopesOmitted && <span className="screen-label env-access-scope">{`${provider.readScopesOmitted} additional captured scopes omitted from this bounded report. No additional scope is implicitly verified.`}</span>}
    <span className="screen-label env-access-key">next grant</span>
    <div className="env-access-value env-access-next">
      {advertised && <div className="settings-segments env-access-lifetimes" role="radiogroup" aria-label={`${provider.provider} access duration`}>
        {options.map(option => <button key={option.key} type="button" role="radio" aria-checked={chosen(option.choice)} disabled={busy}
          className={`screen-chip settings-segment ${chosen(option.choice) ? "chip-chosen" : "cell-action"}`} onClick={() => setChoice(option.choice)}>{option.label}</button>)}
      </div>}
      {choice.kind === "custom" && <div className="env-step-actions">
        <input className="settings-field env-access-amount" inputMode="numeric" aria-label="Custom duration" aria-invalid={custom && "problem" in custom} value={amount} disabled={busy} onChange={e => setAmount(e.target.value)} />
        <div className="settings-segments" role="radiogroup" aria-label="Custom duration unit">
          {(["minutes", "hours"] as const).map(each => <button key={each} type="button" role="radio" aria-checked={unit === each} disabled={busy}
            className={`screen-chip settings-segment ${unit === each ? "chip-chosen" : "cell-action"}`} onClick={() => setUnit(each)}>{each === "minutes" ? "min" : "h"}</button>)}
        </div>
        {custom && "problem" in custom
          ? <span className="mono-bad" role="status">{custom.problem}</span>
          : <span className="screen-label">{`${lifetimeText(limits.minSeconds)} to ${lifetimeText(limits.maxSeconds)}`}</span>}
      </div>}
      <div className="env-step-actions">
        <button type="button" className="screen-chip cell-action" disabled={busy || !ready || !lifetime} onClick={grant}>{`${active ? "renew" : "allow"} ${lifetime ? lifetimePhrase(lifetime) : "…"}`}</button>
        {shorter && <span className="mono-warn">{shorter}</span>}
      </div>
      <span className="screen-label env-access-meaning">{[
        choice.kind === "session"
          ? "This live Wes session only. Ends on revoke, environment disable, credential or binding change, or when Wes closes or restarts. Never restored."
          : "Ends at the deadline, or sooner on revoke, disable, credential or binding change, or session close.",
        "Read proofs end with this access.",
        ...(active ? ["Renewing restarts it from now; read scopes then need a fresh native probe. Nothing is run or resumed."] : []),
      ].join(" ")}</span>
    </div>
    <span className="screen-label env-access-scope">{`Applies to ${provider.provider} only.${sharing.length ? ` ${sharing.join(", ")} share${sharing.length === 1 ? "s" : ""} its credentials but keep${sharing.length === 1 ? "s" : ""} separate access.` : ""}`}</span>
  </div>;
}

/** A locked vault's remembered credentials, unlocked where they are needed. */
function VaultUnlock({ busy, unlock }: { busy: boolean; unlock: (password: string) => Promise<boolean> }) {
  const [password, setPassword] = useState("");
  return <form className="env-auth-credential" aria-label="Unlock credential vault" onSubmit={e => { e.preventDefault(); if (!password || busy) return; const submitted = password; setPassword(""); void unlock(submitted); }}>
    <span className="mono-warn">Remembered credentials are locked.</span>
    <input type="password" autoComplete="current-password" spellCheck={false} className="settings-field" aria-label="Vault password" placeholder="vault password" value={password} disabled={busy} onChange={e => setPassword(e.target.value)} />
    <button className="screen-chip chip-chosen" disabled={busy || !password}>unlock</button>
  </form>;
}

function CredentialInput({ credential, persistenceSupported, disabled, vaultNeeds: needs, openVault, save, forget }: { credential: ProviderAuthentication["credentials"][number]; persistenceSupported: boolean; disabled: boolean; vaultNeeds?: "create" | "unlock"; openVault: (password: string) => Promise<boolean>; save: (value: string, remember: boolean) => Promise<void>; forget: () => Promise<void> }) {
  const [value, setValue] = useState("");
  const [remember, setRemember] = useState(false);
  const [password, setPassword] = useState("");
  const [repeat, setRepeat] = useState("");
  const gated = remember && needs !== undefined;
  const creating = gated && needs === "create";
  const passwordReady = !gated || (creating ? password.length >= MIN_PASSWORD_CHARS && password === repeat : password.length > 0);
  const submit = async () => {
    if (!value || disabled || !passwordReady) return;
    if (gated) {
      const submitted = password;
      setPassword(""); setRepeat("");
      // The value stays in the form until the vault can take it.
      if (!await openVault(submitted)) return;
    }
    const submitted = value; setValue(""); void save(submitted, remember);
  };
  return <form className="env-auth-credential" onSubmit={e => { e.preventDefault(); void submit(); }}>
    <label><span className="mono-ink">{credential.slot}</span><input type="password" autoComplete="new-password" spellCheck={false} className="settings-field" aria-label={`${credential.slot} credential`} placeholder="paste value" value={value} disabled={disabled} onChange={e => setValue(e.target.value)} /></label>
    <label className="env-credential-remember"><input type="checkbox" checked={remember} disabled={disabled || !persistenceSupported} onChange={e => setRemember(e.target.checked)} />Remember on this device</label>
    {!persistenceSupported && <span className="screen-label">Device storage is unavailable on this platform.</span>}
    {gated && <>
      <input type="password" autoComplete={creating ? "new-password" : "current-password"} spellCheck={false} className="settings-field" aria-label="Vault password"
        placeholder={creating ? `new vault password, ${MIN_PASSWORD_CHARS}+ characters` : "vault password"} value={password} disabled={disabled} onChange={e => setPassword(e.target.value)} />
      {creating && <input type="password" autoComplete="new-password" spellCheck={false} className="settings-field" aria-label="Repeat vault password" placeholder="repeat password" value={repeat} disabled={disabled} onChange={e => setRepeat(e.target.value)} />}
      <span className="screen-label">{creating ? "Creates this data folder's credential vault. The password is never stored and cannot be recovered." : "Unlock the credential vault to remember this value."}</span>
    </>}
    <button className="screen-chip chip-chosen" disabled={disabled || !value || !passwordReady}>{credential.present ? "replace" : "supply"}</button>
    <button type="button" className="screen-chip cell-action" disabled={disabled || !credential.present} onClick={() => void forget()}>forget</button>
    <span className={credential.present ? "mono-ok" : "mono-warn"}>{credential.present ? credential.saved ? "saved on device" : "session only" : "missing"}</span>
  </form>;
}
