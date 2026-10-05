/**
 * A font family picked from what the machine has: a chip naming the chosen face, drawn in it, that
 * opens a list with a filter. Each entry is drawn in its own face with a sample that shows what
 * matters for the kind (`0O 1lI {}` tells monospaced faces apart; words tell interface faces apart).
 */
import { useEffect, useRef, useState } from "react";
import { fontCatalogue, fontGroups, type FontFamily } from "./fonts";
import { installedFace } from "./settings-model";

const SAMPLE = { mono: "0O 1lI {} → ✓", sans: "Settings · Setup" } as const;

export function FontPicker({ kind, label, chosen, onPick }: {
  readonly kind: "mono" | "sans";
  readonly label: string;
  readonly chosen: string;
  readonly onPick: (family: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [filter, setFilter] = useState("");
  const [families, setFamilies] = useState<readonly FontFamily[]>([]);
  const holder = useRef<HTMLDivElement>(null);
  const field = useRef<HTMLInputElement>(null);
  useEffect(() => { let live = true; void fontCatalogue().then((read) => { if (live) setFamilies(read); }); return () => { live = false; }; }, []);
  useEffect(() => { if (open) field.current?.focus(); }, [open]);
  useEffect(() => {
    if (!open || typeof document === "undefined") return;
    const away = (event: MouseEvent) => { if (!holder.current?.contains(event.target as Node)) setOpen(false); };
    document.addEventListener("mousedown", away);
    return () => document.removeEventListener("mousedown", away);
  }, [open]);

  const groups = fontGroups(families, kind, chosen, installedFace);
  const wanted = filter.trim().toLowerCase();
  const matches = (name: string) => !wanted || name.toLowerCase().includes(wanted);
  const face = (name: string) => ({ fontFamily: name === "system-ui" ? "system-ui" : `"${name}", ${kind === "mono" ? "monospace" : "sans-serif"}` });
  const pick = (name: string) => { onPick(name); setOpen(false); setFilter(""); };
  const entry = (name: string) => (
    <button key={name} type="button" role="option" aria-selected={name === chosen}
      className={`font-picker-entry${name === chosen ? " font-picker-chosen" : ""}`} onClick={() => pick(name)}>
      <span className="mono-ref font-picker-mark" aria-hidden="true">{name === chosen ? "✓" : ""}</span>
      <span className="font-picker-name" style={face(name)}>{name === "system-ui" ? "System" : name}</span>
      <span className="mono-faint font-picker-sample" style={face(name)}>{SAMPLE[kind]}</span>
    </button>
  );
  const shipped = groups.shipped.filter(matches);
  const installed = groups.installed.filter(matches);

  return <div ref={holder} className="font-picker"
    onKeyDown={(event) => { if (event.key === "Escape" && open) { event.stopPropagation(); setOpen(false); } }}>
    <button type="button" className="screen-chip cell-action font-picker-chip" aria-haspopup="listbox" aria-expanded={open}
      aria-label={`${label}: ${chosen}`} style={face(chosen)} onClick={() => setOpen((was) => !was)}>
      {chosen === "system-ui" ? "System" : chosen} <span className="mono-ref" aria-hidden="true">▾</span>
    </button>
    {open && <div className="font-picker-menu option" role="listbox" aria-label={label}>
      <input ref={field} className="settings-field font-picker-filter" aria-label={`Filter ${label.toLowerCase()}`} value={filter}
        placeholder={`filter · ${groups.shipped.length + groups.installed.length} ${kind === "mono" ? "monospaced" : "proportional"} fonts`}
        onChange={(event) => setFilter(event.target.value)} />
      {shipped.length > 0 && <><span className="screen-label font-picker-group">ships with wes</span>{shipped.map(entry)}</>}
      {installed.length > 0 && <><span className="screen-label font-picker-group">on this machine</span>{installed.map(entry)}</>}
      {shipped.length + installed.length === 0 && <span className="screen-label font-picker-group">no font matches</span>}
    </div>}
  </div>;
}
