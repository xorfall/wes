import { readableIdentity, readableOrigin, type PackageKey, type SpecPackage } from "../api-library";
import type { DraftSummary } from "../draft-api";
import { CopyButton } from "./spec-table";
import { earlierRowsId, HomeCard, HomeReadGate, HomeTitle, libraryGroupId, shortSource, type HomeExpansion, type HomeRead } from "./spec-home";

export type LibraryEntry = { kind: "draft"; value: DraftSummary; label: string } | { kind: "API"; value: SpecPackage; label: string };
/** One API identity with its revisions, latest first. */
export interface LibraryGroup { id: string; key: PackageKey; entries: LibraryEntry[] }

/** A revision's origin as one readable, wrapping source; never the raw storage record. */
export function SpecSource({ origin, className }: { origin: string; className: string }) {
  const readable = readableOrigin(origin);
  if (!readable) return <p className={`${className} mono-faint`}>No source recorded</p>;
  return <p className={className}>{readable.source}{readable.via && <span className="mono-faint">{` · via ${readable.via}`}</span>}</p>;
}

/** Version and scope only when they are facts about the API, not a describe step's storage key. */
export function SpecIdentity({ apiKey }: { apiKey: PackageKey }) {
  const identity = readableIdentity(apiKey);
  return identity ? <p className="spec-card-identity"><span>Version <code>{identity.version}</code></span><span>Scope <code>{identity.scope}</code></span></p> : null;
}

/**
 * Drafts and saved APIs grouped by their full key. Each list is stored in insertion order, so a
 * revision's label is its position within its kind; a saved API that only materializes a draft
 * revision is that draft, not another entry. Drafts remain editable even when a descriptor exists.
 */
export function libraryGroups(drafts: DraftSummary[], packages: SpecPackage[]): LibraryGroup[] {
  const groups = new Map<string, { key: PackageKey; drafts: DraftSummary[]; packages: SpecPackage[] }>();
  const group = (key: PackageKey) => {
    const id = libraryGroupId(key);
    if (!groups.has(id)) groups.set(id, { key, drafts: [], packages: [] });
    return groups.get(id)!;
  };
  drafts.forEach(d => group(d.key).drafts.push(d));
  packages.forEach(p => group(p.key).packages.push(p));
  return [...groups.entries()].map(([id, g]) => ({ id, key: g.key, entries: [
    ...g.drafts.map((value, i): LibraryEntry => ({ kind: "draft", value, label: `r${i + 1}` })).reverse(),
    ...g.packages.map((value, i): LibraryEntry => ({ kind: "API", value, label: `r${i + 1}` }))
      .filter(entry => !g.drafts.some(d => d.descriptorRevision === entry.value.revision)).reverse(),
  ] }));
}

type Readiness = "ready" | "prepare" | "attention";
/** Readiness comes only from validity and a prepared descriptor; review is recorded separately and never blocks. */
export function readiness(entry: LibraryEntry): Readiness {
  if (entry.kind === "API") return "ready";
  return !entry.value.valid ? "attention" : entry.value.descriptorRevision ? "ready" : "prepare";
}
const READINESS: Record<Readiness, { text: string; tone: string; detail: string; note?: string }> = {
  ready: { text: "ready to import", tone: "mono-ok", detail: "valid · prepared for import" },
  prepare: { text: "valid · needs preparation", tone: "mono-warn", detail: "valid · not prepared for import yet", note: "Open the draft and save a revision to prepare it for import." },
  attention: { text: "needs attention", tone: "mono-bad", detail: "saved text isn’t valid", note: "Open the draft to see what needs fixing. Saving keeps invalid text as a new revision." },
};
/** An opened row's detail spans every column of the library table. */
const LIBRARY_COLUMNS = 7;
const kindText =(entry: LibraryEntry) => entry.kind === "draft" ? "draft" : "saved API";
const REVIEWED = " · reviewed";

/** The version chip: the API's real version, with its scope only when another group shares service and version. */
function versionChips(groups: LibraryGroup[]): Map<string, string> {
  const shared = new Map<string, number>();
  const pair = (key: PackageKey) => JSON.stringify([key.service, key.apiVersion]);
  groups.forEach(g => { if (readableIdentity(g.key)) shared.set(pair(g.key), (shared.get(pair(g.key)) ?? 0) + 1); });
  return new Map(groups.flatMap((g): [string, string][] => {
    const identity = readableIdentity(g.key);
    if (!identity) return [];
    return [[g.id, `version ${identity.version}${shared.get(pair(g.key))! > 1 ? ` · ${identity.scope}` : ""}`]];
  }));
}

export function SpecLibrary({ drafts, packages, read, busy, expansion, onDraft, onPackage, onRefresh }: {
  drafts: DraftSummary[]; packages: SpecPackage[]; read: HomeRead; busy: boolean; expansion: HomeExpansion;
  onDraft: (draft: DraftSummary) => void; onPackage: (spec: SpecPackage) => void; onRefresh: () => void;
}) {
  const groups = libraryGroups(drafts, packages);
  const chips = versionChips(groups);
  const ready = read.state === "ready";
  const attention = groups.filter(g => readiness(g.entries[0]!) === "attention").length;
  const open = (entry: LibraryEntry) => entry.kind === "draft" ? onDraft(entry.value) : onPackage(entry.value);
  return <HomeCard label="API library" head={<>
    <HomeTitle title="Library" count={ready ? groups.length : undefined} />
    {ready && attention > 0 && <><span className="mono-faint">·</span><span className="mono-bad">{`${attention} needs attention`}</span></>}
    <span className="spec-home-note spec-home-wide">editable drafts and saved APIs, shared by every workspace</span>
    <span className="spec-home-fill" />
    {read.state !== "failed" && <button type="button" className="screen-chip cell-action spec-home-icon" aria-label="Refresh library" disabled={busy} onClick={onRefresh}>
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" focusable="false">
        <path d="M20 7v5h-5M20 12a8 8 0 1 0-2.34 5.66" />
      </svg>
    </button>}
  </>}>
    <HomeReadGate read={read} what="the library" empty={groups.length === 0} emptyText="No APIs yet. Import OpenAPI below to create an editable draft." busy={busy} onRetry={onRefresh}>
      <div className="spec-home-t" role="table" aria-label="Library APIs">
        <div className="spec-home-row spec-home-lib" role="row">
          <div className="spec-home-th" role="columnheader" /><div className="spec-home-th" role="columnheader">api</div>
          <div className="spec-home-th spec-home-wide" role="columnheader">kind</div><div className="spec-home-th spec-home-wide" role="columnheader">rev</div>
          <div className="spec-home-th" role="columnheader">status</div><div className="spec-home-th spec-home-wide" role="columnheader">source</div>
          <div className="spec-home-th spec-home-wide" role="columnheader" aria-label="Action" />
        </div>
        {groups.map(g => <LibraryRow key={g.id} group={g} chip={chips.get(g.id)} busy={busy} expansion={expansion} onOpen={open} />)}
      </div>
      <p className="spec-home-key spec-home-wide">
        <span className="mono-ok">ready to import</span><span className="spec-home-label">saved and prepared</span>
        <span className="mono-warn">valid · needs preparation</span><span className="spec-home-label">valid, not prepared for import yet</span>
        <span className="mono-bad">needs attention</span><span className="spec-home-label">the saved text isn’t valid</span>
        <span className="spec-home-label">· review is optional and never blocks</span>
      </p>
    </HomeReadGate>
  </HomeCard>;
}

function LibraryRow({ group, chip, busy, expansion, onOpen }: {
  group: LibraryGroup; chip: string | undefined; busy: boolean; expansion: HomeExpansion; onOpen: (entry: LibraryEntry) => void;
}) {
  const { key, entries } = group;
  const latest = entries[0]!;
  const earlier = entries.slice(1);
  const state = READINESS[readiness(latest)];
  const expanded = expansion.open.has(group.id);
  const earlierId = earlierRowsId(group.id);
  const earlierOpen = expansion.open.has(earlierId);
  const source = readableOrigin(latest.value.origin);
  const identity = readableIdentity(key);
  const name = `${key.service}${chip ? ` ${chip}` : ""}`;
  const toggle = () => expansion.onToggle(group.id);
  const openLabel = `open ${latest.kind === "draft" ? "draft" : "API"}`;
  const status = `${state.text}${latest.value.accepted ? REVIEWED : ""}`;
  return <div className={expanded ? "spec-home-open" : undefined} role="rowgroup">
    <div className="spec-home-row spec-home-lib" role="row">
      <div className="spec-home-td spec-home-c-chev" role="cell"><button type="button" className="spec-home-chev" aria-expanded={expanded} aria-label={`${expanded ? "Collapse" : "Expand"} ${name}`} onClick={toggle}>{expanded ? "▾" : "▸"}</button></div>
      <div className="spec-home-td" role="cell" aria-description={name}>
        <span className="spec-home-line"><button type="button" className="spec-home-name" tabIndex={-1} onClick={toggle}>{key.service}</button>{chip && <span className="spec-home-chip">{chip}</span>}</span>
        <span className="spec-home-line spec-home-narrow mono-dim">{`${kindText(latest)} ${latest.label}`}</span>
      </div>
      <div className="spec-home-td spec-home-wide" role="cell">{kindText(latest)}</div>
      <div className="spec-home-td spec-home-wide mono-dim" role="cell">{latest.label}</div>
      <div className="spec-home-td spec-home-c-status" role="cell" aria-description={status}><span className={state.tone}>{state.text}</span>{latest.value.accepted && <span className="mono-dim">{REVIEWED}</span>}</div>
      <div className="spec-home-td spec-home-wide mono-dim" role="cell" aria-description={source?.source}>{source ? shortSource(source.source) : <span className="mono-faint">not recorded</span>}</div>
      <div className="spec-home-td spec-home-c-act spec-home-wide" role="cell"><button type="button" className={`screen-chip ${expanded ? "chip-chosen" : "cell-action"}`} disabled={busy} aria-label={`Open ${key.service} ${latest.kind} ${latest.label}`} onClick={() => onOpen(latest)}>{openLabel}</button></div>
    </div>
    {expanded && <div role="row"><div className="spec-home-detail" role="cell" aria-colspan={LIBRARY_COLUMNS}>
      <span className="spec-home-label">source</span>
      <span className="spec-home-full">{source ? <>{source.source}{source.via && <span className="mono-dim">{` · via ${source.via}`}</span>}<span className="spec-home-copy"><CopyButton text={source.source} label="Copy source" /></span></> : <span className="mono-faint">not recorded</span>}</span>
      {identity && <><span className="spec-home-label">identity</span><span className="spec-home-full">{`version ${identity.version} · scope ${identity.scope}`}</span></>}
      <span className="spec-home-label">latest</span>
      <span className="spec-home-full">{`${kindText(latest)} ${latest.label}`}<span className="mono-dim">{` · ${state.detail}${latest.value.accepted ? REVIEWED : ""}`}</span></span>
      {latest.kind === "draft" && state.note && <><span className="spec-home-label" /><span className="spec-home-label">{state.note}</span></>}
      <span className="spec-home-label">earlier<span className="spec-home-narrow"> revisions</span></span>
      <div className="spec-home-earlier">
        {earlier.length === 0 ? <span className="mono-dim">none</span> : <>
          <button type="button" className="spec-home-link" aria-expanded={earlierOpen} onClick={() => expansion.onToggle(earlierId)}>{`${earlierOpen ? "▾" : "▸"} ${earlier.length} earlier ${earlier.length === 1 ? "revision" : "revisions"}`}</button>
          {earlierOpen && <div className="spec-home-t spec-home-sub" role="table" aria-label={`Earlier revisions of ${name}`}>
            <div className="spec-home-row" role="row">
              <div className="spec-home-th" role="columnheader">rev</div><div className="spec-home-th" role="columnheader">kind</div><div className="spec-home-th" role="columnheader">status</div>
              <div className="spec-home-th spec-home-wide" role="columnheader">review</div><div className="spec-home-th" role="columnheader" aria-label="Action" />
            </div>
            {earlier.map(entry => {
              const at = READINESS[readiness(entry)];
              const from = entry.value.origin !== latest.value.origin ? readableOrigin(entry.value.origin) : undefined;
              return <div className="spec-home-row" role="row" key={`${entry.kind}:${entry.value.revision}`}>
                <div className="spec-home-td mono-dim" role="cell">{entry.label}</div>
                <div className="spec-home-td" role="cell">{kindText(entry)}</div>
                <div className="spec-home-td spec-home-c-status" role="cell"><span className={at.tone}>{at.text}</span>{entry.value.accepted && <span className="mono-dim spec-home-narrow">{REVIEWED}</span>}</div>
                <div className="spec-home-td spec-home-wide mono-dim" role="cell">{entry.value.accepted ? "reviewed" : "—"}</div>
                <div className="spec-home-td spec-home-c-act spec-home-c-wrap" role="cell">
                  <button type="button" className="screen-chip cell-action" disabled={busy} aria-label={`Open ${key.service} ${entry.kind} ${entry.label}`} onClick={() => onOpen(entry)}>open</button>
                  {from && <span className="spec-home-from mono-dim">{`from ${from.source}${from.via ? ` · via ${from.via}` : ""}`}</span>}
                </div>
              </div>;
            })}
          </div>}
        </>}
      </div>
      <div className="spec-home-narrow spec-home-detail-act"><button type="button" className="screen-chip chip-chosen" disabled={busy} onClick={() => onOpen(latest)}>{openLabel}</button></div>
    </div></div>}
  </div>;
}
