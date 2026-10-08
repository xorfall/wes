import {defineView} from "@wes/view-sdk";
import {definition,type Input} from "./contract";
import "./view.css";

type Comparison=Input["comparisons"][number];
type Tone="ok"|"warn"|"unknown";
export interface Fact {label:string;text:string;tone:Tone}

/** The comparison's stated hypothesis in plain words. An unrecognised hypothesis is shown as given. */
const HYPOTHESIS:Readonly<Record<string,string>>={
  incomparable:"Not comparable",
  different_outcomes:"Different observed outcomes",
  same_observed_outcome:"Same observed outcome · not a verified fix",
  suspected_intermittence:"Suspected intermittence · cause unknown",
};
const PREVIEW_ROWS=2;
const ROLE:Record<Tone,string>={ok:"status-ok",warn:"status-warn",unknown:"screen-label"};

/**
 * What each comparison establishes, read only from its declared fields. Whether the target ran,
 * whether the input matched and whether the environment is known to match are separate facts; none
 * is inferred from another, from the hypothesis text or from the run names. A missing environment
 * is unknown, never equal.
 */
export function facts(c:Comparison):Fact[] {
  return [
    c.targetExercised
      ?{label:"target",text:"ran in both executions",tone:"ok"}
      :{label:"target",text:"not exercised in both · outcomes are not comparable",tone:"warn"},
    c.inputComparable
      ?{label:"input",text:"same captured input",tone:"ok"}
      :{label:"input",text:"not shown to be the same",tone:"warn"},
    c.environmentComparable===null
      ?{label:"environment",text:"unknown",tone:"unknown"}
      :c.environmentComparable
        ?{label:"environment",text:"same captured environment",tone:"ok"}
        :{label:"environment",text:"differs",tone:"warn"},
    c.regressionRuledOut
      ?{label:"regression",text:"ruled out by this comparison",tone:"ok"}
      :{label:"regression",text:"not ruled out",tone:"unknown"},
  ];
}

/** A comparison whose outcomes cannot be compared: the target did not run in both, or the input differs. */
export const comparable=(c:Comparison)=>c.targetExercised&&c.inputComparable;

export function prepare(input:Input){
  const rows=input.comparisons.map(c=>({comparison:c,facts:facts(c),comparable:comparable(c),hypothesis:HYPOTHESIS[c.hypothesis]??c.hypothesis}));
  return {
    title:input.title,
    rows,
    incomparable:rows.filter(r=>!r.comparable).length,
    notExercised:input.comparisons.filter(c=>!c.targetExercised).length,
    environmentUnknown:input.comparisons.filter(c=>c.environmentComparable===null).length,
  };
}

const plural=(n:number,one:string,many:string)=>`${n} ${n===1?one:many}`;

export default defineView(definition,{Component:({input,context})=>{
  const model=prepare(input);
  const preview=context.mode==="preview";
  const rows=preview?model.rows.slice(0,PREVIEW_ROWS):model.rows;
  return <section className="execution-comparison" aria-label={model.title}>
    <h3 className="execution-comparison-title">{model.title}</h3>
    <p className="execution-comparison-counts screen-label">
      {model.rows.length===0?"No comparisons.":plural(model.rows.length,"comparison","comparisons")}
      {model.incomparable>0&&<> · <span className="status-warn">{model.incomparable} not comparable</span></>}
      {model.notExercised>0&&<> · <span className="status-warn">{model.notExercised} target not exercised</span></>}
      {model.environmentUnknown>0&&<> · {model.environmentUnknown} environment unknown</>}
    </p>
    {rows.length>0&&<ol className="execution-comparison-list">
      {rows.map(({comparison:c,facts:said,comparable:ok,hypothesis},at)=><li key={`${c.subject}\u0000${c.baseline}\u0000${c.target}\u0000${at}`}
        className="execution-comparison-item" data-comparable={ok}>
        <div className="execution-comparison-head">
          <strong className="execution-comparison-target" title={c.target}>{c.target}</strong>
          <span className={`execution-comparison-hypothesis${ok?"":" status-warn"}`}>{hypothesis}</span>
        </div>
        <div className="execution-comparison-runs screen-label">
          <span>subject <code title={c.subject}>{c.subject}</code></span>
          <span>baseline <code title={c.baseline}>{c.baseline}</code></span>
        </div>
        <ul className="execution-comparison-facts" aria-label={`What the comparison of ${c.subject} and ${c.baseline} establishes`}>
          {said.map(f=><li key={f.label}><span className="screen-label">{f.label}</span> <span className={ROLE[f.tone]}>{f.text}</span></li>)}
        </ul>
        {!preview&&<p className="execution-comparison-rationale">{c.rationale}</p>}
      </li>)}
    </ol>}
    {preview&&model.rows.length>rows.length&&<small className="screen-label">+{model.rows.length-rows.length} comparisons</small>}
    {!preview&&<p className="execution-comparison-note screen-label" role="note">A matching input or source tree does not establish the same provider, environment, fix or cause.</p>}
  </section>;
}});
