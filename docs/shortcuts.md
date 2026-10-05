# Keyboard shortcuts

Keyboard handling belongs to the focused surface. Cell letter shortcuts do not
run while typing or interacting with result controls. Embedded views and terminals receive their own keys. The View SDK forwards
Ctrl/Cmd+Tab, Enter, R and M to the host; Ctrl+Tab cycles tabs and Ctrl/Cmd+M opens cell actions when the frame is inside
a cell. Forwarded Enter/R do not themselves run workspace commands. Other parent shortcuts, including Escape and pane expansion,
do not cross the frame boundary. A shortcut only offers actions available for that run or value.

`Mod` means Cmd on macOS and Ctrl on Windows/Linux. Cmd is written explicitly
where a shortcut currently requires the Meta key rather than the platform's
primary editing modifier. Platform verification status is in the README.

## Command input

| Keys | Action |
| --- | --- |
| Enter | Accept an active suggestion; otherwise submit. |
| Mod+Enter | Submit without accepting a suggestion. |
| Shift+Enter | Insert a new line at the selection. |
| Mod+Shift+Enter | Open the exact draft in the workspace editor. |
| Tab | Accept a suggestion, or request suggestions. |
| Shift+Tab | Move keyboard focus backwards. |
| Ctrl+Space | Request suggestions. |
| Up / Down | Browse suggestions; otherwise recall history at the first/last line with no selected text. |
| Escape | Dismiss suggestions first. |
| Ctrl+Shift+C | Toggle cell action labels between keys and controls. |

Ctrl+C is left to the native text control. It does not cancel a running command.
Modifier combinations and IME composition that are not listed stay with editing.
IME composition does not submit or move a draft to the editor.

## Focused command cell

Click or focus the cell itself, rather than an input or a button within it.

| Keys | Action |
| --- | --- |
| j / k | Focus next / previous cell. |
| r | Repeat when available; configured confirmation still applies. |
| e / b | Edit / branch. |
| x | Cancel or stop the selected run, with the scope shown by its action label. |
| p | Pin / unpin the cell in the scrollback. Does not Keep or Pin input. |
| d / h | Open run details / history. |
| Space | Cycle collapsed, preview and expanded result sizes. |
| o / v | Inspect / open the JSON tab. |
| w | Open the offered view, if available. |
| y | Copy the selected result's Wes path. |
| c | Copy the command source, when offered. |
| f | Follow the session's newest output, when offered; separate from log follow. |
| [ / ] | Select the previous / next result in a cell with multiple results. |
| Shift+D | Open the delete-work flow. Does not bypass its review. |
| Mod+M | Open / close the cell actions menu. Escape also closes it; Ctrl+M works on macOS too. |

**Pin input** is a view control that retains the displayed input and binds the
view to that fixed result. **Stop observing** pauses that view's reads.
Neither is the cell's `p` shortcut, nor does either cancel an external source.

## Panes and tabs

| Keys | Action |
| --- | --- |
| Alt+1 … Alt+4 | Focus a pane, outside text editing. |
| Alt+W | Close the focused pane, outside text editing. |
| Ctrl+Tab / Ctrl+Shift+Tab | Cycle that pane's tabs. |
| Alt+Left / Alt+Right | Cycle tabs outside text-editing controls. |
| Cmd+L | Focus the visible command input in the active pane (Meta key). |
| Cmd+arrows | Move focus to a neighboring pane (Meta key). |
| Cmd+Shift+F | Expand the focused pane when multiple panes exist (Meta key). |
| Escape while a pane is expanded | Restore the pane layout before handling any inner Escape action. |
| Left / Right / Home / End on a tab button | Move focus through the tab row. Enter or Space activates it. |
| Delete on a closable tab button | Close that tab. |

Escape otherwise belongs to the inner surface first. Use the visible **Back to
session** / **Close dashboard** buttons when leaving a dashboard, including
when focus is inside an embedded view. Exiting a dashboard does not stop sources.

## Editors

| Where | Keys | Action |
| --- | --- | --- |
| Workspace editor | Mod+Enter | Run the whole draft. |
| Workspace editor | Mod+R | Repeat its bound node. |
| Workspace editor | Mod+/ | Toggle a comment. |
| YAML editor | Mod+S | Save the current text. |
| YAML editor | Mod+R | Run its configured action, if available. |
| YAML editor | Shift+Enter | Insert a context-aware new line. |
| Source editor | Ctrl+Space / Enter | Request completion / accept an active completion. |
| Either editor | Escape | Dismiss completion first; otherwise use that editor's leave action. |

The source editors use Tab / Shift+Tab for indentation. Escape dismisses an
open completion list first; press it again to return from the editor. The
workspace editor preserves its draft when returning to the session.

The API draft editor additionally uses Mod+S to save, Mod+Enter to check,
Shift+Alt+F to reindent, and Mod+Space to request completion. Its surrounding
review panel has separate controls; these do not run while typing in its source.

## Host conflicts

A browser or operating system can reserve combinations before Wes receives them.
For example, Cmd+L and Ctrl+Tab also belong to browser navigation,
Ctrl+Shift+C opens browser inspection, and Ctrl+Space can switch an input
method, and the API draft editor's Cmd+Space may open Spotlight. Use `/theme cell keys` or `/theme cell controls` to change action labels
if the host reserves their shortcut. The desktop
menu gives Mod+M to cell actions instead of macOS window minimization.
Buttons and slash commands remain available when a host consumes a shortcut.
Terminal copy, interrupts and editing keys belong to the terminal, not the
command-input or cell tables above. Custom view packages may define their own
local controls.
