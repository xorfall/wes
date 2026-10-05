import type { Extension } from "@codemirror/state";
import { drawSelection } from "@codemirror/view";

/** Let CodeMirror reconcile the caret with its document instead of native WebKit painting. */
export function editorSelection(): Extension {
  // Match ordinary text selection: a caret when collapsed, a highlight when selecting.
  return drawSelection({ drawRangeCursor: false });
}
