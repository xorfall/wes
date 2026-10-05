/**
 * A cell-level notice — `ENV000`, a warning, the `:env plan` document notice — drawn line by line as
 * the engine wrote it, never parsed. The preview spends at most six lines and counts the rest in the
 * tail; expanded shows every line.
 */
import { useEffect } from "react";
import type { Mode } from "../../presentation/types";
import { PREVIEW_LINES } from "../../presentation/types";
import { useBlockReport } from "../Cell";
import { MonoLine } from "../MonoLine";

export function NoticeBlock({ lines, severity, mode }: { readonly lines: readonly string[]; readonly severity: "info" | "warning"; readonly mode: Mode }) {
  const shown = mode === "preview" ? lines.slice(0, PREVIEW_LINES) : lines;
  const left = lines.length - shown.length;
  const report = useBlockReport();
  useEffect(() => {
    report(left > 0 ? { counts: [{ text: `+${left} line${left === 1 ? "" : "s"}`, role: "mono-faint" }], offers: [] } : undefined);
  }, [left, report]);
  const tone = severity === "warning" ? "mono-warn" : "mono-dim";
  return <div className="value-notice cell-document-notice" aria-label="Command messages">
    {shown.map((text, at) => <MonoLine key={at} segments={[
      ...(at === 0 ? [{ text: "ⓘ ", role: severity === "warning" ? "mono-warn" as const : "mono-meta" as const }] : []),
      { text, role: tone },
    ]} className="value-line" />)}
  </div>;
}
