/**
 * A screen summoned by name.
 *
 * Screens are navigation surfaces rather than execution output: `/graph`, `/env`, `/settings` and `/open` open over
 * the workspace or in a pane, and `esc` returns to the session with the half-typed line intact.
 * They all wear the same chrome — the session's top bar, then the screen's name beside whatever it
 * is about, then the screen, then a footer that always begins by saying how to leave.
 *
 * Screen chrome is sans, because it is chrome; the data inside it is mono, because it is data.
 */
import { useEffect, useRef, type ReactNode } from "react";
import { MonoLine, type Segment } from "./MonoLine";
import { composing } from "../platform-keys";
import "./surface.css";
import "./screens.css";

export interface ScreenProps {
  /** `/graph`, `/env`, `/settings`, `/open`. */
  readonly name: string;
  /** The session's own top line, so the screen never loses where it is. */
  readonly top: readonly Segment[];
  /** What this screen is about: the node, the result, the count of environments. */
  readonly subject?: readonly Segment[];
  /** Between the subject and the body: a toolbar, tabs, whatever the screen offers. */
  readonly tools?: ReactNode;
  /** The keys this screen answers to, at the far right of its head where the eye ends up. */
  readonly keys?: readonly Segment[];
  readonly children: ReactNode;
  /** Always starts with how to leave. */
  readonly footer: readonly Segment[];
  readonly onClose?: () => void;
  /**
   * `full` over the workspace; `pane` inside one.
   *
   * A screen in a pane keeps its tools and its body and drops the rest: the pane's head already
   * says what it holds, and the split's own footer already says how to leave. Saying either twice
   * would cost the pane the room the screen came for.
   */
  readonly chrome?: "full" | "pane";
}

/** `esc back to the session, the half-typed line intact`, and then whatever else the screen offers. */
export function leaving(...rest: readonly Segment[]): Segment[] {
  return [
    { text: "esc", role: "mono-ref" },
    { text: " back to the session, the half-typed line intact", role: "mono-dim" },
    ...(rest.length > 0 ? ([{ text: "   ", role: "mono-faint" }] as Segment[]) : []),
    ...rest,
  ];
}

export function Screen({ name, top, subject, tools, keys, children, footer, onClose, chrome = "full" }: ScreenProps) {
  const section = useRef<HTMLElement | null>(null);
  /*
   * A summoned screen takes the focus.
   *
   * The prompt had it and the prompt is no longer drawn, so without this nothing on the page has
   * it, and `esc back to the session` — which every screen's footer promises — reaches nothing.
   * The keys the screen offers are in the same position.
   */
  useEffect(() => {
    const element = section.current;
    const pane = element?.closest(".split-pane");
    if (element && !element.closest("[hidden]") && (!pane || pane.getAttribute("aria-current") === "true")
      && !element.contains(document.activeElement)) {
      element.focus({ preventScroll: true });
    }
  }, [chrome]);

  const Frame = chrome === "full" ? "section" : "div";
  return (
    <Frame
      ref={element => { section.current = element; }}
      className={chrome === "full" ? "screen" : "screen screen-in-pane"}
      aria-label={name}
      tabIndex={-1}
      onKeyDown={(event) => {
        // Controls own the first refusal (completion, graph selection, etc.); a composition owns its Escape.
        if (event.defaultPrevented || event.key !== "Escape" || composing(event)) return;
        event.preventDefault();
        event.stopPropagation();
        onClose?.();
      }}
    >
      {chrome === "full" && <>
        <div className="screen-top surface-sunk">
          <MonoLine segments={top} className="screen-top-line" />
        </div>
        <div className="screen-head">
          <span className="screen-title">{name}</span>
          {subject && <MonoLine segments={subject} className="screen-subject" />}
          {keys && <MonoLine segments={keys} className="screen-keys" />}
        </div>
      </>}
      {tools && <div className="screen-tools">{tools}</div>}
      <div className="screen-body">{children}</div>
      {chrome === "full" && <MonoLine segments={footer} className="screen-footer" />}
    </Frame>
  );
}
