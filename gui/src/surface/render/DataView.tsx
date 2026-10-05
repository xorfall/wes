/**
 * Arbitrary data — an HTTP trace event's details, a panel's value — through the same presentation
 * pipeline as a result, in the window's paged mode. Small values only: nothing is cached, and
 * preparing them is cheap.
 */
import { useMemo, useRef } from "react";
import type { TypeShape } from "../../protocol";
import { prepareSync } from "../../presentation/prepare";
import { present } from "../../presentation/present";
import { window_lines } from "../../presentation/types";
import { useColumns } from "./measure";
import { Presented } from "./Presentation";
import { useRegistry } from "./ValueBlock";

export function DataView({ type = { kind: "unknown" }, data, lines = window_lines() }: { readonly type?: TypeShape; readonly data: unknown; readonly lines?: number }) {
  const box = useRef<HTMLDivElement>(null);
  const columns = useColumns(box);
  const registry = useRegistry();
  const presentation = useMemo(() => present({
    prepared: prepareSync({ type, data }), registry,
    context: { mode: "window", columns, lines, density: "normal", locale: "en-GB", timeZone: Intl.DateTimeFormat().resolvedOptions().timeZone },
  }), [type, data, columns, lines, registry]);
  return <div ref={box} className="value-block"><Presented node={presentation.root} /></div>;
}
