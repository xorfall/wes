import type { Cell } from "../cells";
import type { Workspace } from "../workspace";
import { resultNamed } from "./open-model";

export interface DefinitionOwner { readonly pane: string; readonly workspace?: string; readonly cells: readonly string[] }
export type RegisterDefinition = (owner: DefinitionOwner) => () => void;
export interface DefinitionJump { readonly pane: string; readonly cell: string; readonly revision: number; readonly source?: Cell }

/** Resolve only acknowledged work in this workspace; navigation never creates a run. */
export function definitionTarget(workspace: Workspace, cells: readonly Cell[], name: string): Cell {
  const named = resultNamed(workspace, name, "/goto");
  if (named.trouble) throw new Error(named.trouble);
  const cell = cells.find(cell => cell.nodes.includes(named.node!));
  if (!cell) throw new Error(`The definition of $${name} is no longer available in this workspace's session history.`);
  return cell;
}
