import { EnvironmentProviders, useEnvironmentAuthentication } from "../EnvironmentAuthentication";
import type { AuthWorkspace, ProviderAuthentication } from "../../environment-authentication";
import { chipGroups, missingCredentials, shortRevision, targetsOf, withAuthentication } from "../environment-model";
/**
 * `/env` — which environment a command runs against, and what that means.
 *
 * One card per environment: the radio line that chooses it (its name and one word for each of its
 * states), then what it holds. A card opens and closes on its own disclosure, apart from choosing:
 * the chosen card opens by itself, any other opens without being chosen. Closed, a card shows its
 * providers as chips grouped by kind, marking the ones that can change real systems and the ones
 * missing credentials; open, it shows the providers' table, where each authenticated provider opens
 * its own setup. The environment that can change real systems wears the failed rail, because that
 * is the fact the eye should find first.
 *
 * Choosing that one asks again, inside its own card. Not because the click might have been an
 * accident, but because the answer is recorded: every cell run under it is marked.
 */
import { useEffect, useId, useState } from "react";
import { MonoLine, type Segment } from "../MonoLine";
import { leaving, Screen } from "../Screen";
import "../environment.css";

export interface EnvironmentProvider {
  readonly name: string;
  /** Where it comes from (an API spec, a built-in, a process…), when the service says. */
  readonly kind?: string;
  /** The execution target it runs on, when the service says. */
  readonly target?: string;
  /** The API endpoint it contacts, apart from where it runs, when the service says. */
  readonly endpoint?: string;
  /** It has commands that can change real systems. */
  readonly writes: boolean;
  /** The credentials it declares and whether each is supplied. Presence never grants access. */
  readonly credentials: readonly { readonly name: string; readonly supplied: boolean }[];
  /** Its HTTP authentication contract and setup, when it has one. */
  readonly authentication?: ProviderAuthentication;
}

export interface Environment {
  readonly name: string;
  readonly providers: readonly EnvironmentProvider[];
  /** How many of its providers' credentials are supplied, out of how many they need. */
  readonly credentials?: { readonly present: number; readonly wanted: number };
  /** The whole revision: the identity requests use. The screen shows its short form. */
  readonly revision?: string;
  /** Whether it may run; unknown when the engine did not say. */
  readonly execution?: boolean;
  /** It can change real systems: choosing it asks again, and marks every cell run under it. */
  readonly dangerous?: boolean;
}

export interface EnvProps {
  readonly authentication?: AuthWorkspace;
  readonly top: readonly Segment[];
  readonly environments: readonly Environment[];
  readonly chosen: string;
  readonly onChoose?: (name: string) => void;
  readonly onClear?: () => void;
  readonly onEnable?: (name: string) => void;
  readonly onClose?: () => void;
  /** `pane` when the screen is inside a split rather than over the workspace. */
  readonly chrome?: "full" | "pane";
}

export function envHeader(environments: readonly Environment[], chosen = ""): Segment[] {
  const count = environments.length;
  return [
    { text: `${count} ${count === 1 ? "environment" : "environments"}`, role: "mono-ink" },
    ...(chosen ? [{ text: " · ", role: "mono-faint" as const }, { text: chosen, role: "mono-ref" as const }, { text: " selected", role: "mono-faint" as const }] : []),
  ];
}

/** Independent facts: execution, effects and material presence never imply a grant. */
export function statusOf(environment: Environment): (Segment & { title: string })[] {
  const badges: (Segment & { title: string })[] = [];
  if (environment.execution !== undefined) badges.push({
    text: environment.execution ? "enabled" : "disabled",
    role: environment.execution ? "mono-ok" : "mono-faint",
    title: `Execution ${environment.execution ? "enabled" : "disabled"}. Credential access is granted separately.`,
  });
  if (environment.dangerous) badges.push({ text: "writes", role: "mono-bad", title: "Contains commands that can change real systems. No operation is implied by this status." });
  const credentials = environment.credentials;
  badges.push(credentials ? {
    text: `credentials ${credentials.present}/${credentials.wanted}`,
    role: credentials.present < credentials.wanted ? "mono-warn" : "mono-dim",
    title: `${credentials.present} of ${credentials.wanted} credential values supplied. Presence does not grant access.`,
  } : { text: "no credentials", role: "mono-dim", title: "No credential slots are currently bound in this environment." });
  return badges;
}

/** Where its providers run, when they said, and its revision, on one wrapping line. */
export function factsOf(environment: Environment): Segment[] {
  const dot: Segment = { text: " · ", role: "mono-faint" };
  const targets = targetsOf(environment);
  return [
    ...(environment.providers.length === 0 ? [{ text: "no providers", role: "mono-dim" as const }] : []),
    ...(targets.length ? [{ text: `runs on ${targets.join(", ")}`, role: "mono-dim" as const }] : []),
    ...(environment.revision ? [{ text: `rev ${shortRevision(environment.revision)}`, role: "mono-faint" as const }] : []),
  ].flatMap((segment, at) => at === 0 ? [segment] : [dot, segment]);
}

/** The sentence under the cards, which is the whole reason the effectful one asks again. */
export function envNote(): string {
  return "Selection applies to new drafts; existing drafts keep their captured context. Environments that write for real ask once more. Selecting never enables execution or grants authority.";
}

/** What the chips' marks mean, said once under the cards. */
export function chipLegend(): { mark: string; meaning: string }[] {
  return [{ mark: "writes", meaning: "can change real systems" }, { mark: "missing", meaning: "credentials missing" }];
}

/** What the card asks before it lets a command change real systems. */
export function confirmation(name: string): string {
  return `Select ${name}? Its commands can change real systems. Every cell run under it is marked.`;
}

/** What a chip says to assistive technology: its name, then each mark in words. */
function chipLabel(provider: EnvironmentProvider): string {
  return [provider.name, ...(provider.writes ? ["can change real systems"] : []), ...(missingCredentials(provider) ? ["credentials missing"] : [])].join(", ");
}

/** A closed card's providers, a bounded row of chips per kind; `+N more` opens the card. */
function ProviderChips({ environment, onMore }: { environment: Environment; onMore: () => void }) {
  return <div className="env-chips" aria-label={`${environment.name} providers`}>
    {chipGroups(environment.providers).map(group => <span key={group.kind ?? ""} className="env-chip-group">
      {group.kind && <span className="env-chip-kind">{group.kind}</span>}
      {group.providers.map(provider => <span key={provider.name} className="env-chip" aria-label={chipLabel(provider)} aria-description={chipLabel(provider)}>
        {provider.name}
        {provider.writes && <span className="env-chip-mark env-mark-writes" aria-hidden="true" />}
        {missingCredentials(provider) && <span className="env-chip-mark env-mark-missing" aria-hidden="true" />}
      </span>)}
      {group.hidden > 0 && <button type="button" className="env-chip env-chip-more" onClick={onMore}>{`+${group.hidden} more`}</button>}
    </span>)}
  </div>;
}

export function EnvScreen({ top, environments, chosen, onChoose, onClear, onEnable, onClose, chrome = "full", authentication }: EnvProps) {
  const [asking, setAsking] = useState<string | undefined>(undefined);
  const ids = useId();
  // Opened or closed by hand; a card without an entry is open exactly when it is the chosen one.
  const [disclosed, setDisclosed] = useState<Readonly<Record<string, boolean>>>({});
  useEffect(() => {
    setAsking(undefined);
    setDisclosed(was => { const { [chosen]: _, ...rest } = was; return rest; });
  }, [chosen]);
  const reload = environments.map(environment => `${environment.name}@${environment.revision ?? ""}:${environment.execution ?? ""}`).join("|");
  const auth = useEnvironmentAuthentication(authentication, reload);
  const shown = environments.map(environment => withAuthentication(environment, auth.report));

  const isOpen = (name: string) => disclosed[name] ?? name === chosen;
  const disclose = (name: string, open: boolean) => setDisclosed(was => ({ ...was, [name]: open }));
  const discloseAll = (open: boolean) => setDisclosed(Object.fromEntries(environments.map(environment => [environment.name, open])));
  const confirm = (name: string) => { setAsking(undefined); if (name !== chosen) onChoose?.(name); };
  const choose = (environment: Environment) => {
    if (environment.name === chosen) { setAsking(undefined); return; }
    if (environment.dangerous && asking !== environment.name) return setAsking(environment.name);
    confirm(environment.name);
  };

  const tools = <div className="spec-tools">
    {onClear && chosen && <button type="button" className="screen-chip cell-action" onClick={onClear}>clear selection</button>}
    {environments.length > 0 && <>
      <button type="button" className="screen-chip cell-action" onClick={() => discloseAll(true)}>expand all</button>
      <button type="button" className="screen-chip cell-action" onClick={() => discloseAll(false)}>collapse all</button>
    </>}
  </div>;

  return (
    <Screen
      name="/env"
      top={top}
      chrome={chrome}
      subject={envHeader(environments, chosen)}
      tools={tools}
      footer={leaving()}
      onClose={() => (asking && asking !== chosen ? setAsking(undefined) : onClose?.())}
    >
      <div className="env-rows" role="radiogroup" aria-label="Environments">
        {shown.length === 0 && <p className="screen-label">No environment definitions received.</p>}
        {shown.map((environment, index) => {
          const here = environment.name === chosen;
          const open = isOpen(environment.name);
          const body = `${ids}-providers-${index}`;
          return (
            <article key={environment.name} className={`env-card ${here ? "option-chosen" : "option"}${environment.dangerous ? " env-card-dangerous" : ""}`} aria-label={environment.name}>
              {environment.dangerous && <div className="env-rail rail-failed" aria-hidden="true" />}
              <div className="env-card-head">
                <button type="button" className="env-disclosure" aria-expanded={open} aria-controls={body}
                  aria-label={`${open ? "Collapse" : "Expand"} ${environment.name}`} onClick={() => disclose(environment.name, !open)}>{open ? "▾" : "▸"}</button>
                <div
                  className="env-row"
                  role="radio"
                  aria-checked={here}
                  tabIndex={0}
                  onClick={() => choose(environment)}
                  onKeyDown={(event) => {
                    if (event.key === "ArrowUp" || event.key === "ArrowDown") {
                      event.preventDefault();
                      const rows = event.currentTarget.closest(".env-rows")?.querySelectorAll<HTMLElement>('[role="radio"]');
                      rows?.[(index + (event.key === "ArrowDown" ? 1 : -1) + shown.length) % shown.length]?.focus();
                      return;
                    }
                    if (event.key !== "Enter" && event.key !== " ") return;
                    event.preventDefault();
                    event.stopPropagation();
                    choose(environment);
                  }}
                >
                  <span className={`env-radio${here ? " on" : ""}`} aria-hidden="true" />
                  <div className="env-said">
                    <div className="env-headline">
                      <MonoLine segments={[{ text: environment.name, role: here ? "mono-ink-strong" : "mono-ink" }]} className="env-name" />
                      <div className="env-status">{statusOf(environment).map(badge => <span key={badge.text} className={`env-badge ${badge.role}`} aria-description={badge.title} aria-label={badge.title}>{badge.text}</span>)}</div>
                    </div>
                  </div>
                </div>
              </div>
              <div className="env-card-body">
                {!open && environment.providers.length > 0 && <ProviderChips environment={environment} onMore={() => disclose(environment.name, true)} />}
                <p className="env-facts">{factsOf(environment).map((segment, at) => <span key={at} className={segment.role}>{segment.text}</span>)}</p>
              </div>
              {here && environment.execution === false && onEnable && <div className="env-execution-controls">
                <button type="button" className="screen-chip cell-action" onClick={() => onEnable(environment.name)}>enable environment</button>
              </div>}
              {asking === environment.name && !here && (
                <div className="env-asking" role="alert">
                  <p className="mono-bad">{confirmation(environment.name)}</p>
                  <button type="button" className="screen-chip chip-chosen" onClick={() => confirm(environment.name)}>{`select ${environment.name}`}</button>
                  <button type="button" className="screen-chip cell-action" onClick={() => setAsking(undefined)}>cancel</button>
                </div>
              )}
              <div id={body} className="env-card-providers" hidden={!open}>
                {open && <EnvironmentProviders environment={environment} {...(authentication ? { state: auth } : {})} />}
              </div>
            </article>
          );
        })}
      </div>
      {shown.some(environment => environment.providers.length > 0) && <p className="screen-label env-legend">
        {chipLegend().map(({ mark, meaning }) => <span key={mark} className="env-legend-item"><span className={`env-chip-mark env-mark-${mark}`} aria-hidden="true" />{meaning}</span>)}
      </p>}
      <p className="screen-label env-note">{envNote()}</p>
    </Screen>
  );
}
