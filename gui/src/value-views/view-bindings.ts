import { createContext } from "react";
import type { Engine } from "../engine";
import type { ViewFrame } from "./instances";

/** Exactly what the host drew for one View member: the frame it came from and its revisions. */
export interface ViewBinding {
  readonly engine: Engine;
  readonly generation: string;
  /** The frame root's node and instance identity, as the frame was read. */
  readonly authorityEpoch?: string;
  readonly root: string;
  readonly rootInstance: string;
  /** The drawn member and the revisions it was drawn at. */
  readonly member: string;
  readonly revision: string;
  readonly inputRevision: string;
  /** Input fields bound from other results; the server reads only the member's own input. */
  readonly linkedInputs: readonly string[];
}

/** The binding of the member drawn at `view/{id}` in the frame currently presented, if any. */
export type ViewBindings = (path: string) => ViewBinding | undefined;
export const ViewBindingContext = createContext<ViewBindings | undefined>(undefined);

/** Bindings for every member of `frame`, read from that frame only. */
export function frameBindings(frame: ViewFrame, rootInstance: string, engine: Engine, generation: string): ViewBindings {
  const members = new Map(frame.instances.map(entry => [`view/${entry.id}`, entry]));
  return path => {
    const entry = members.get(path);
    if (!entry) return undefined;
    return { engine, generation, authorityEpoch: frame.authorityEpoch, root: frame.root, rootInstance, member: entry.id, revision: entry.revision, inputRevision: entry.inputRevision, linkedInputs: entry.linkedInputs };
  };
}
