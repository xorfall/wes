import {defineView} from "@wes/view-sdk";
import {definition} from "./contract";
import "./view.css";

export default defineView(definition, {
  initial: input => ({selected: input.items[0] ?? ""}),
  reduce: (_state, event) => ({selected: event.selected}),
  outputs: state => ({selected: state.selected}),
  Component: ({input, state, emit}) => <section className="task-board">
    <h3 className="screen-title">{input.title}</h3>
    <p className="screen-label">Selected: <span className="table-value">{state.selected || "none"}</span></p>
    <div>{input.items.map(item => <button key={item} type="button"
      aria-pressed={state.selected === item} onClick={() => emit({selected: item})}>{item}</button>)}</div>
  </section>,
});
