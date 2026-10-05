import { useId } from "react";
import { describeSourceKind, quoted } from "../api-library";
import { HomeCard } from "./spec-home";

/** A describe name becomes a provider identifier. */
export const DESCRIBE_NAME = /^[A-Za-z_][A-Za-z0-9_]*$/;
const UNSENT_SEPARATOR = " · ";

/** The session command for one describe: a `scheme://` location is a URL, anything else a local file. */
export function describeCommand(location: string, provider: string): string {
  return `:describe ${describeSourceKind(location)}:${quoted(location)} provider:${provider}`;
}

/**
 * Import OpenAPI: a disclosure whose typed values belong to the landing, so collapsing it never
 * loses them. Sending hands the command to the session; the draft appears in the library only when
 * the job has published it, so nothing here claims success.
 */
export function SpecDescribe({ location, provider, open, accent, busy, onLocation, onProvider, onToggle, onDescribe }: {
  location: string; provider: string; open: boolean; accent: boolean; busy: boolean;
  onLocation: (value: string) => void; onProvider: (value: string) => void; onToggle: () => void; onDescribe: (command: string) => void;
}) {
  const id = useId();
  const nameValid = DESCRIBE_NAME.test(provider);
  const nameProblem = provider.length > 0 && !nameValid;
  const unsent = [location, provider].filter(Boolean).join(UNSENT_SEPARATOR);
  return <HomeCard label="Import OpenAPI" accent={accent && open} head={<>
    <button type="button" className="spec-home-chev" aria-expanded={open} aria-controls={`${id}-form`} aria-label={`${open ? "Collapse" : "Expand"} OpenAPI import`} onClick={onToggle}>{open ? "▾" : "▸"}</button>
    <button type="button" className="spec-home-title spec-home-titlebtn" tabIndex={-1} onClick={onToggle}>Import OpenAPI</button>
    {!open && (unsent
      ? <span className="mono-dim spec-home-unsent" aria-description={unsent}>{`· unsent: ${unsent}`}</span>
      : <span className="spec-home-note">from an OpenAPI JSON or YAML URL or file</span>)}
  </>}>
    <form id={`${id}-form`} className="spec-home-describe" hidden={!open} onSubmit={e => { e.preventDefault(); if (location && nameValid) onDescribe(describeCommand(location, provider)); }}>
      <div className="spec-home-fields">
        <label className="spec-home-label" htmlFor={`${id}-source`}>source</label>
        <div className="spec-home-field">
          <input id={`${id}-source`} aria-label="OpenAPI source" className="spec-home-input" value={location} onChange={e => onLocation(e.target.value)} placeholder="https://… or a local file path" required />
          <span className="spec-home-label">OpenAPI 3.0/3.1 JSON and YAML are parsed locally without a model. Documentation pages are not supported.</span>
        </div>
        <label className="spec-home-label" htmlFor={`${id}-name`}>name</label>
        <div className="spec-home-field">
          <input id={`${id}-name`} aria-label="Describe provider" className="spec-home-input spec-home-input-name" value={provider} onChange={e => onProvider(e.target.value)} placeholder="acme" required
            aria-invalid={nameProblem} {...(nameProblem ? { "aria-describedby": `${id}-name-problem` } : {})} />
          {nameProblem && <span id={`${id}-name-problem`} className="mono-bad spec-home-problem">Use letters, digits and _; start with a letter or _.</span>}
        </div>
      </div>
      <div className="spec-home-send">
        <button type="submit" className="screen-chip chip-chosen" disabled={busy || !location || !nameValid}>create draft</button>
        <span className="spec-home-label"><span className="spec-home-wide">Creates an editable draft in the library. Nothing is imported and the API isn’t called.</span><span className="spec-home-narrow">creates a draft; imports nothing</span></span>
      </div>
    </form>
  </HomeCard>;
}
