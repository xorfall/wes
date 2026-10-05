/**
 * Shared parts of the /spec landing: the env-style section frame, the read state each section owns,
 * row identities for expand/collapse, and the short form of a long source for a closed row.
 */
import type { ReactNode } from "react";
import type { PackageKey } from "../api-library";

/** One independent read shown on the landing: still reading, read, or failed with its reason. */
export type HomeRead = { state: "loading" } | { state: "ready" } | { state: "failed"; message: string };
export const READING: HomeRead = { state: "loading" };
export const READ: HomeRead = { state: "ready" };

/** Which rows (and earlier-revision lists) are open; the landing owns it so expand/collapse all can reach every section. */
export interface HomeExpansion { open: ReadonlySet<string>; onToggle: (id: string) => void }

/** A workspace import is identified by environment and alias together. */
export const workspaceRowId = (spec: { environment: string; alias: string }) => `workspace:${JSON.stringify([spec.environment, spec.alias])}`;
/** A library group is identified by its full package key. */
export const libraryGroupId = (key: PackageKey) => `library:${JSON.stringify([key.service, key.apiVersion, key.scope])}`;
export const earlierRowsId = (groupId: string) => `earlier:${groupId}`;

const SHORT_SEGMENTS = 3;
/** A path or address cut to its last two segments for a closed row; the full text stays in the hint and the details. */
export function shortSource(text: string): string {
  const parts = text.split("/");
  return parts.length > SHORT_SEGMENTS ? `…/${parts.slice(-2).join("/")}` : text;
}

/** A section frame: the title line (title, count, notes, tools) over its content. */
export function HomeCard({ label, head, accent, children }: { label: string; head: ReactNode; accent?: boolean; children?: ReactNode }) {
  return <section className={`spec-home-card option${accent ? " spec-home-card-accent" : ""}`} aria-label={label}>
    <header className="spec-home-head">{head}</header>
    {children}
  </section>;
}

/** A section's title and count; the count is a dash until the section has been read. */
export function HomeTitle({ title, count }: { title: string; count: number | undefined }) {
  return <><h2 className="spec-home-title">{title}</h2><span className="mono-faint">·</span><span className="mono-dim">{count ?? "—"}</span></>;
}

/** Loading, a read failure and a true empty section are different states, each said plainly. */
export function HomeReadGate({ read, what, empty, emptyText, busy, onRetry, children }: {
  read: HomeRead; what: string; empty: boolean; emptyText: string; busy: boolean; onRetry: () => void; children: ReactNode;
}) {
  if (read.state === "loading") return <p className="spec-home-label">{`Reading ${what}…`}</p>;
  if (read.state === "failed") return <>
    <div className="spec-home-strip" role="alert"><span className="mono-bad">{`Couldn’t read ${what}.`}</span><button type="button" className="screen-chip cell-action" disabled={busy} onClick={onRetry}>try again</button></div>
    <p className="spec-home-label spec-home-full">{read.message}</p>
  </>;
  if (empty) return <p className="spec-home-label">{emptyText}</p>;
  return <>{children}</>;
}
