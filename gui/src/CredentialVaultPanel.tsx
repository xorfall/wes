/**
 * The credential vault, as a row of `/settings data`: what keeps remembered credentials on the
 * left, the password field and its one action on the right, and lock and reset beside them. A
 * reset asks once more in the row itself; it removes remembered credentials and nothing else.
 */
import { useState } from "react";
import { MIN_PASSWORD_CHARS, useCredentialVault, type VaultState } from "./credential-vault";

const STATE_TEXT: Record<VaultState, string> = { absent: "not created", locked: "locked", unlocked: "unlocked" };

export function CredentialVaultPanel() {
  const vault = useCredentialVault();
  const [password, setPassword] = useState("");
  const [confirmation, setConfirmation] = useState("");
  const [resetting, setResetting] = useState(false);
  const report = vault.report;
  if (!report) return vault.message ? <div className="settings-row" aria-label="Credential vault">
    <div className="settings-row-head"><span className="screen-label settings-label">Credential vault</span></div>
    <div className="settings-block"><p className="mono-line settings-status mono-bad" role="status">{vault.message.text}</p></div>
  </div> : null;
  if (report.kind === "system") return <div className="settings-row" aria-label="Credential vault">
    <div className="settings-row-head"><span className="screen-label settings-label">Credential vault</span></div>
    <div className="settings-block"><p className="settings-note">Remembered credentials are kept in this computer's system secure store, outside the data folder.</p></div>
  </div>;
  const state = report.state;
  const creating = state === "absent";
  const short = password.length < MIN_PASSWORD_CHARS;
  const mismatch = creating && password !== confirmation;
  const submit = async () => {
    if (state === "unlocked" || vault.busy || (creating ? short || mismatch : !password)) return;
    const submitted = password;
    setPassword(""); setConfirmation("");
    await vault.act({ action: creating ? "create" : "unlock", password: submitted }, creating ? "Credential vault created and unlocked." : "Credential vault unlocked.");
  };
  return <form className="settings-row" aria-label="Credential vault" onSubmit={event => { event.preventDefault(); void submit(); }}>
    <div className="settings-row-head">
      <span className="screen-label settings-label">Credential vault</span>
      <code className={`mono-line settings-command ${state === "unlocked" ? "mono-ok" : "mono-warn"}`}>{STATE_TEXT[state]}</code>
    </div>
    <div className="settings-block">
      {state !== "unlocked" && <div className="settings-field-row">
        <input type="password" autoComplete={creating ? "new-password" : "current-password"} spellCheck={false} className="settings-field" aria-label="Vault password"
          placeholder={creating ? `new password, ${MIN_PASSWORD_CHARS}+ characters` : "vault password"} value={password} disabled={vault.busy} onChange={event => setPassword(event.target.value)} />
        {creating && <input type="password" autoComplete="new-password" spellCheck={false} className="settings-field" aria-label="Repeat vault password"
          placeholder="repeat password" value={confirmation} disabled={vault.busy} onChange={event => setConfirmation(event.target.value)} />}
        <button type="submit" className="screen-chip chip-chosen" disabled={vault.busy || (creating ? short || mismatch : !password)}>{creating ? "Create vault" : "Unlock"}</button>
      </div>}
      {(state !== "absent") && <div className="settings-field-row">
        {state === "unlocked" && <button type="button" className="screen-chip cell-action" disabled={vault.busy} onClick={() => void vault.act({ action: "lock" }, "Credential vault locked.")}>Lock</button>}
        {resetting
          ? <>
            <span className="mono-warn">Remove every remembered credential?</span>
            <button type="button" className="screen-chip chip-chosen" disabled={vault.busy} onClick={() => { setResetting(false); void vault.act({ action: "reset", confirm: "reset" }, "Credential vault removed. Workspaces and data are unchanged."); }}>Confirm reset</button>
            <button type="button" className="screen-chip cell-action" disabled={vault.busy} onClick={() => setResetting(false)}>Cancel</button>
          </>
          : <button type="button" className="screen-chip cell-action" disabled={vault.busy} onClick={() => setResetting(true)}>Reset vault</button>}
      </div>}
      {creating && mismatch && confirmation && <p className="mono-line settings-status mono-warn">The passwords differ.</p>}
      <p className="settings-note">Credentials you choose to remember are encrypted in this data folder with this password. The password is never stored: the vault starts locked every time the application starts.</p>
      <p className="settings-note">There is no recovery. A forgotten password can only be reset, which removes remembered credentials and keeps workspaces and results.</p>
      {vault.message && <p className={`mono-line settings-status ${vault.message.failed ? "mono-bad" : "mono-dim"}`} role="status">{vault.message.text}</p>}
    </div>
  </form>;
}
