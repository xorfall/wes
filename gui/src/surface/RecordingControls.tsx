/**
 * The recording commands the engine says this run accepts — status, Stop for an attached writer,
 * Discard for an unused prepared setup — as one action row shared by the cell, `/open` and the
 * inspector. No source command (start, cancel) is ever offered here.
 *
 * An action only writes the exact command into the session prompt for review; opening, drawing or
 * reading never submits anything. Commands name the recording by its node id, not by a name that
 * could be rebound to other work, and by the exact run the engine stated the control for; the engine
 * checks ownership and that the run is still current again when they are submitted.
 * Only what the engine stated for the node's current run is offered: nothing is inferred from the
 * command text, the progress kind, or a Dataset or a copy of one.
 */
import { useContext } from "react";
import type { WorkspaceNode } from "../workspace";
import { CANONICAL_RUN as RUN, ComposeContext, freshName, REFERABLE_NODE as REFERABLE } from "./dataset-management";
import { MonoLine } from "./MonoLine";
import "./dataset-management.css";

export interface RecordingActions { readonly status: boolean; readonly stop: boolean; readonly discard: boolean; readonly run: string }

/** What may be offered for this node now, for the exact run the engine stated it for, or nothing. */
export function recordingActions(node: WorkspaceNode | undefined): RecordingActions | undefined {
  const control = node?.recordingControl;
  if (!node || !control || node.run === undefined || control.run !== node.run || node.accessWithdrawn) return undefined;
  if (!REFERABLE.test(node.id) || !RUN.test(control.run)) return undefined;
  return { status: control.statusAvailable, stop: control.stopAvailable, discard: control.discardAvailable, run: control.run };
}

/*
 * Every command carries the run it was prepared for, so reviewed text never acts on a later run of
 * the same node: the engine refuses it once that run is no longer current, before anything changes.
 */
export const statusCommand = (node: string, run: string, name: string) => `:dataset recording-status $${node} run:"${run}" > ${name}`;
export const stopCommand = (node: string, run: string, name: string) => `:dataset stop $${node} run:"${run}" > ${name}`;
export const discardCommand = (node: string, run: string, name: string) => `:dataset discard $${node} run:"${run}" > ${name}`;

const PREPARED = "Commands are prepared in the prompt; nothing runs until you submit.";
export const STOP_NOTE = `Stop recording ends this recording and keeps what it committed; its source keeps running. ${PREPARED}`;
export const DISCARD_NOTE = `Discard removes this unused recording setup; it neither starts nor cancels the source. Once attached, a recording is ended with Stop. ${PREPARED}`;

export function RecordingControls({ node }: { readonly node: WorkspaceNode | undefined }) {
  const composer = useContext(ComposeContext);
  const actions = recordingActions(node);
  if (!node || !actions) return null;
  const status = composer ? freshName("status", composer.taken) : undefined;
  const stopped = composer ? freshName("stopped", composer.taken) : undefined;
  const discarded = composer ? freshName("discarded", composer.taken) : undefined;
  const note = !composer ? "recording commands are prepared in the session"
    : actions.stop ? STOP_NOTE
    : actions.discard ? DISCARD_NOTE
    : actions.status ? PREPARED
    : "no recording command is available for this run now";
  return <div className="management-actions recording-controls" role="group" aria-label="Recording controls">
    {composer && actions.status && <button type="button" className="cell-action" disabled={!status}
      onClick={() => status && composer.compose(statusCommand(node.id, actions.run, status))}>recording status…</button>}
    {composer && actions.stop && <button type="button" className="cell-action" disabled={!stopped}
      onClick={() => stopped && composer.compose(stopCommand(node.id, actions.run, stopped))}>stop recording…</button>}
    {composer && actions.discard && <button type="button" className="cell-action" disabled={!discarded}
      onClick={() => discarded && composer.compose(discardCommand(node.id, actions.run, discarded))}>discard recording setup…</button>}
    <MonoLine segments={[{ text: note, role: "mono-faint" }]} className="value-line" description={note} />
  </div>;
}
