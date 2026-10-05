import { EditorState } from "@codemirror/state";
import { EditorView, keymap, lineNumbers } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { autocompletion, completionKeymap, type CompletionContext } from "@codemirror/autocomplete";
import { linter } from "@codemirror/lint";
import { jsonSyntax } from "./json-syntax";

export function specCompletion(context:CompletionContext) {
  const word=context.matchBefore(/[A-Za-z_]*/);if(!word||(!context.explicit&&word.from===word.to))return null;
  const line=context.state.doc.lineAt(context.pos).text;
  const methods=/"method"\s*:/.test(line);
  const names=methods?["GET","HEAD","POST","PUT","PATCH","DELETE","OPTIONS","TRACE"]:["version","provider","types","operations","path","summary","method","route","auth","parameters","responses","evidence","name","wire","location","type","required","encoding","scheme","secret"];
  return {from:word.from,options:names.map(label=>({label,type:methods?"constant":"property"}))};
}
export function specExtensions(change:(text:string)=>void,save:()=>void,validate:()=>void,readOnly=false) {
  return [jsonSyntax,lineNumbers(),history(),EditorState.readOnly.of(readOnly),EditorView.editable.of(!readOnly),
    keymap.of([{key:"Mod-s",run:()=>{save();return true;}},{key:"Mod-Enter",run:()=>{validate();return true;}},...completionKeymap,...defaultKeymap,...historyKeymap,indentWithTab]),
    autocompletion({override:[specCompletion]}),
    linter(view=>{try{JSON.parse(view.state.doc.toString());return [];}catch{return [{from:0,to:Math.min(1,view.state.doc.length),severity:"error",message:"Invalid JSON. Validate to see descriptor diagnostics."}];}}),
    EditorView.updateListener.of(update=>{if(update.docChanged)change(update.state.doc.toString());}),
    EditorView.contentAttributes.of({"aria-label":"Spec JSON source"}),
    EditorView.theme({"&":{fontFamily:"var(--type-mono-family)",fontSize:"13px",backgroundColor:"transparent"},".cm-scroller":{overflow:"auto",maxHeight:"50vh"},".cm-content":{padding:"12px"},".cm-gutters":{backgroundColor:"transparent",border:"none",color:"var(--mono-faint)"}}),
  ];
}
export function makeSpecEditor(parent:HTMLElement,source:string,change:(text:string)=>void,save:()=>void,validate:()=>void,readOnly=false):EditorView {
  return new EditorView({parent,state:EditorState.create({doc:source,extensions:specExtensions(change,save,validate,readOnly)})});
}
