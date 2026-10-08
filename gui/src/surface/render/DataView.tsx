/**
 * Arbitrary data — an HTTP trace event's details, a panel's value — through the same presentation
 * pipeline as a result, in the window's paged mode, with its captured contract metadata when given. Small values only: nothing is cached, and
 * preparing them is cheap.
 */
import { useMemo, useRef } from "react";
import type { StoredValue, TypeShape } from "../../protocol";
import { prepareSync } from "../../presentation/prepare";
import { present } from "../../presentation/present";
import { window_lines } from "../../presentation/types";
import { useColumns } from "./measure";
import { Presented } from "./Presentation";
import { useRegistry } from "./ValueBlock";
import { DatasetHostContext, type DatasetHost } from "./dataset-source";

export function DataView({ type = { kind: "unknown" }, data, meta, lines = window_lines() }: { readonly type?: TypeShape; readonly data: unknown; readonly meta?: StoredValue["meta"]; readonly lines?: number }) {
  const box = useRef<HTMLDivElement>(null);
  const columns = useColumns(box);
  const registry = useRegistry();
  const presentation = useMemo(() => present({
    prepared: prepareSync({ type, data, ...(meta ? { meta } : {}) }), registry,
    context: { mode: "window", columns, lines, density: "normal", locale: "en-GB", timeZone: Intl.DateTimeFormat().resolvedOptions().timeZone },
  }), [type, data, meta, columns, lines, registry]);
  // This data is not a stored result: any Dataset inside it is shown with its descriptor and the
  // reason its records are not read here, never read through an enclosing result's handle.
  return <DatasetHostContext.Provider value={DETACHED}><div ref={box} className="value-block"><Presented node={presentation.root} /></div></DatasetHostContext.Provider>;
}

const DETACHED: DatasetHost = { mode: "window", collapsed: false };
