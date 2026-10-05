import {defineView} from "@wes/view-sdk";
import {definition} from "./contract";
import "./view.css";

/** Preview is a bounded summary; expanded and window modes show the full composition. */
const PREVIEW_MEMBER_LIMIT = 2;

export default defineView(definition,{Component:({input,slots,context})=>{
  const members = slots.members ?? [];
  const shown = context.mode === "preview" ? members.slice(0, PREVIEW_MEMBER_LIMIT) : members;
  const hidden = members.length - shown.length;
  return <section className="dashboard" aria-label={input.title}>
    <h2 className="screen-title">{input.title}</h2><div className="dashboard-members">{shown}</div>
    {hidden > 0&&<p className="screen-label">{hidden} more {hidden === 1 ? "view" : "views"} — expand to see the rest.</p>}
    {!members.length&&<p className="screen-label">Connect Views with :view connect.</p>}
  </section>;
}});
