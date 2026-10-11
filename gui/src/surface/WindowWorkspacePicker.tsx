import { useEffect, useRef, useState } from "react";
import { openWorkspace } from "../workspace-binding";
import type { Workspace } from "../workspace";

/** Explicit opening binds this document only, without creating workspaces or changing the session. */
export function WindowWorkspacePicker({ binding, workspace, onChoose }: { binding?: string; workspace: Workspace; onChoose(name: string): void }) {
  const current = binding ?? workspace.identity?.name;
  const directory = useRef<readonly string[]>([]);
  if (workspace.identity) directory.current = workspace.identity.saved;
  const names = [...new Set([...(current ? [current] : []), ...directory.current])];
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string>();
  const attempt = useRef(0);
  useEffect(() => () => { attempt.current++; }, []);
  useEffect(() => { attempt.current++; setBusy(false); setProblem(undefined); }, [current]);
  const owner = useRef(current); owner.current = current;
  const choose = async (name: string) => {
    if (!name || name === current || busy) return;
    const ticket = ++attempt.current, before = current;
    setBusy(true); setProblem(undefined);
    try {
      await openWorkspace(name, false);
      if (ticket === attempt.current && owner.current === before) onChoose(name);
    } catch (error) { if (ticket === attempt.current && owner.current === before) setProblem(error instanceof Error ? error.message : String(error)); }
    finally { if (ticket === attempt.current && owner.current === before) setBusy(false); }
  };
  return <div className="window-workspace-toolbar">
    <label>Workspace <select className="settings-field" aria-label="Window workspace" value={current ?? ""} disabled={busy || names.length === 0} onChange={event => void choose(event.target.value)}>
      {!current && <option value="">Select workspace</option>}
      {names.map(name => <option key={name} value={name}>{name}</option>)}
    </select></label>
    {busy && <span role="status" className="mono-faint">Opening workspace…</span>}
    {problem && <span role="status" className="mono-warn">{problem}</span>}
    {workspace.identity === undefined && current && <span className="mono-faint">Waiting for {current}</span>}
  </div>;
}
