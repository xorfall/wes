import { DiagnosticsPanel } from "../../DiagnosticsPanel";
/**
 * `/settings` — the client's settings, by section.
 *
 * In the command sections every row is a command you can type, and the footer says so. Each such
 * setting shows the command that changes it beside its name, so the screen teaches the command line
 * rather than replacing it: somebody who reads this once never has to come back. The limits section
 * is the exception: its operating budgets are saved for the next launch of the app, shared by every
 * workspace, and edited in the panel that section mounts rather than from the line.
 *
 * The options are cards rather than a list, because each one has a name and a consequence and the
 * consequence is the part worth reading. A row is the setting's name and its command on the left,
 * its cards on the right, so the names line up and the cards get the room; a narrow pane stacks
 * the two.
 *
 * The sections are tabs across the top, and they are also commands: `/settings keys` opens the
 * one the tab does. Tabs stay in a pane, where the screen's head does not, because a section that
 * cannot be reached is not a section.
 */
import { useId, type KeyboardEvent } from "react";
import { DataHomePanel } from "../../DataHomePanel";
import { CredentialVaultPanel } from "../../CredentialVaultPanel";
import { LimitsSettings } from "../../limits/LimitsSettings";
import { FontPicker } from "../FontPicker";
import { MonoLine, type Segment } from "../MonoLine";
import { leaving, Screen } from "../Screen";

export interface Option {
  readonly name: string;
  /** What choosing it means: `light · warm`, `rail only`, `1.62 · normal`. */
  readonly what: string;
}

export interface SettingRow {
  /** Which setting this row writes. `settings-model.ts` reads it; the screen only passes it back. */
  readonly key?: string;
  /** `choice` (the default): a fixed set drawn as segmented chips; `font`: a family picked from the machine. */
  readonly kind?: "choice" | "font";
  /** For a font row: monospaced (data) or proportional (interface) families. */
  readonly family?: "mono" | "sans";
  /** Said under the label: what the row governs. */
  readonly note?: string;
  readonly label: string;
  /** The command this row is: `/theme paper`. */
  readonly command: string;
  readonly options: readonly Option[];
  readonly chosen: string;
}

export interface SettingsProps {
  readonly top: readonly Segment[];
  /** The screen's sections, in order. The chosen one shows its rows. */
  readonly sections: readonly string[];
  readonly section: string;
  readonly rows: readonly SettingRow[];
  /**
   * What is set or configured, for a section this screen does not choose for.
   *
   * Appearance is the section with cards, because the surface's own settings are the ones the
   * client decides. The rest say what is actually there — the keys, the providers, what is being
   * kept — which is the honest thing to show and better than an empty list of controls.
   */
  readonly facts?: readonly (readonly Segment[])[];
  /** Said instead, when this section has neither a choice nor a fact. */
  readonly empty?: string;
  /** A mono line drawn in the chosen face and palette, so a choice can be seen before it is made. */
  readonly preview?: readonly Segment[];
  readonly onChoose?: (row: SettingRow, option: Option) => void;
  /** A tab was chosen; the owner opens that section, the way `/settings <section>` would. */
  readonly onSection?: (section: string) => void;
  readonly onClose?: () => void;
  /** `pane` when the screen is inside a split rather than over the workspace. */
  readonly chrome?: "full" | "pane";
}

/** The section whose panel edits operating budgets rather than offering commands. */
const LIMITS_SECTION = "limits";
const COMMAND_FOOTER = "every row here is a command you can type";
const LIMITS_FOOTER = "budgets saved here apply from the next launch, in every workspace";

/** Which tab the arrow keys land on: the row wraps, Home and End are its ends. */
export function tabAfter(sections: readonly string[], from: string, key: string): string | undefined {
  const at = sections.indexOf(from);
  if (at < 0 || sections.length === 0) return undefined;
  if (key === "ArrowRight") return sections[(at + 1) % sections.length];
  if (key === "ArrowLeft") return sections[(at - 1 + sections.length) % sections.length];
  if (key === "Home") return sections[0];
  if (key === "End") return sections[sections.length - 1];
  return undefined;
}

export function SettingsScreen({
  top, sections, section, rows, facts = [], empty, preview, onChoose, onSection, onClose, chrome = "full",
}: SettingsProps) {
  const id = useId();
  const tabId = (name: string) => `${id}-tab-${name}`;
  const panelId = `${id}-section`;
  const limits = section === LIMITS_SECTION;
  const onTabKey = (event: KeyboardEvent<HTMLButtonElement>) => {
    const next = tabAfter(sections, section, event.key);
    if (next === undefined) return;
    event.preventDefault(); event.stopPropagation();
    onSection?.(next);
    (event.currentTarget.parentElement?.querySelector<HTMLElement>(`[id="${tabId(next)}"]`))?.focus();
  };
  const tabs = (
    <div className="settings-tabs" role="tablist" aria-label="Settings sections">
      {sections.map((name) => {
        const here = name === section;
        return (
          <button key={name} type="button" role="tab" id={tabId(name)} className="settings-tab"
            aria-selected={here} aria-controls={panelId} tabIndex={here ? 0 : -1}
            aria-description={`/settings ${name}`}
            onClick={() => { if (!here) onSection?.(name); }}
            onKeyDown={onTabKey}>{name}</button>
        );
      })}
    </div>
  );
  return (
    <Screen
      name="/settings"
      top={top}
      chrome={chrome}
      tools={tabs}
      onClose={onClose}
      footer={leaving({ text: limits ? LIMITS_FOOTER : COMMAND_FOOTER, role: "mono-faint" })}
    >
      <div className="settings-rows" role="tabpanel" id={panelId} aria-labelledby={tabId(section)}>
        {section === "data" && <DataHomePanel />}
        {section === "data" && <CredentialVaultPanel />}
        {limits && <LimitsSettings />}
        {rows.map((row) => (
          <div className="settings-row" key={row.label}>
            <div className="settings-row-head">
              <span className="screen-label settings-label">{row.label}</span>
              {row.note && <span className="screen-label settings-note-line">{row.note}</span>}
              <MonoLine segments={[{ text: row.command, role: "mono-meta" }]} className="settings-command" description="type this at the prompt to set it from the line" />
            </div>
            {row.kind === "font"
              ? <div className="settings-choice">
                <FontPicker kind={row.family ?? "mono"} label={row.label} chosen={row.chosen} onPick={(family) => onChoose?.(row, { name: family, what: "" })} />
              </div>
              : <div className="settings-choice">
                <div className="settings-segments" role="radiogroup" aria-label={row.label}>
                  {row.options.map((option) => {
                    const here = option.name === row.chosen;
                    return (
                      <button key={option.name} type="button" role="radio" aria-checked={here}
                        className={`screen-chip settings-segment ${here ? "chip-chosen" : "cell-action"}`}
                        onClick={() => onChoose?.(row, option)}>{option.name}</button>
                    );
                  })}
                </div>
                {(() => { const chosen = row.options.find((option) => option.name === row.chosen); return chosen ? <span className="screen-label settings-meaning">{`${chosen.name} · ${chosen.what}`}</span> : null; })()}
              </div>}
          </div>
        ))}
        {facts.length > 0 && (
          <div className="settings-facts">
            {facts.map((line, at) => (
              <MonoLine key={at} segments={line} className="settings-fact" />
            ))}
          </div>
        )}
        {section === "data" && <DiagnosticsPanel />}
        {rows.length === 0 && facts.length === 0 && section !== "data" && !limits && (
          <MonoLine segments={[{ text: empty ?? "nothing to set yet", role: "mono-faint" }]} className="settings-empty" />
        )}
        {preview && (
          <div className="settings-row">
            <div className="settings-row-head">
              <span className="screen-label settings-label">Preview</span>
            </div>
            <div className="settings-preview option">
              <MonoLine segments={preview} />
            </div>
          </div>
        )}
      </div>
    </Screen>
  );
}
