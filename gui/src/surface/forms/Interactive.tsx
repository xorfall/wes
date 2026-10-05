/**
 * A process that stopped to ask something.
 *
 * It asks in place. Sending the person somewhere else to answer would lose the thing the cell is
 * for: the question is about this command, and the answer belongs to the same line of work.
 */
import { MonoLine } from "../MonoLine";
import type { FormValue } from "./form";
import "../surface.css";

export interface InteractiveModel {
  readonly question: string;
  /** Whether the answer should be hidden as it is typed. */
  readonly secret: boolean;
  /**
   * The lines before the question, newest last.
   *
   * A process asking something has usually been writing for a while, and the last line on its own
   * is a question with its context cut off — `event 20 emitted` says nothing about 1 to 19. The
   * engine reports the conversation as active for as long as the run lasts and never says the
   * moment it is waiting, so the honest reading is: the tail is what it has written, and the last
   * line of it is what it is asking.
   */
  readonly before?: readonly string[];
}

export interface InteractiveProps {
  readonly model: InteractiveModel;
  readonly answer?: string;
  readonly onAnswer?: (text: string) => void;
  readonly onSend?: () => void;
  readonly disabled?: boolean;
}

export function InteractivePreview({ model, answer = "", onAnswer, onSend, disabled = false }: InteractiveProps) {
  return (
    <div className="form-lines">
      {(model.before ?? []).map((line, at) => (
        <MonoLine key={at} segments={[{ text: line, role: "mono-literal" }]} />
      ))}
      <MonoLine segments={[{ text: model.question, role: "mono-ink" }]} />
      <input
        className="form-answer mono-ink"
        type={model.secret ? "password" : "text"}
        aria-label={model.question}
        placeholder="type an answer, enter to send"
        value={answer}
        disabled={disabled}
        autoComplete="off"
        onChange={(event) => onAnswer?.(event.target.value)}
        onKeyDown={(event) => {
          event.stopPropagation();
          if (event.key === "Escape") {
            event.preventDefault();
            event.currentTarget.closest<HTMLElement>("[data-cell]")?.focus();
            return;
          }
          if (event.key !== "Enter") return;
          if (event.nativeEvent?.isComposing) return;
          event.preventDefault();
          // The cell's own keys must not fire while an answer is being typed into it.
          event.stopPropagation();
          onSend?.();
        }}
      />
    </div>
  );
}

/** How much of what the process wrote is kept above the question. */
const BEFORE = 3;

export function readInteractive(value: FormValue): InteractiveModel {
  const question = value.asking ?? "";
  const written = (value.wrote ?? "").replace(/\n$/, "").split("\n").filter((line) => line.trim() !== "");
  // The question is the last line it wrote, so it is not also one of the lines before it.
  const before = written[written.length - 1] === question ? written.slice(0, -1) : written;
  return { question, secret: false, before: before.slice(-BEFORE) };
}
