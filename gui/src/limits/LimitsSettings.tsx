import {useCallback,useEffect,useRef,useState} from 'react';
import {LimitsPanel} from './LimitsPanel';
import {readBudgets,saveBudgets,type BudgetSnapshot} from './policy';
/** Transport and ownership controller for the Limits settings panel. */
export function LimitsSettings() {
  const [snapshot,setSnapshot]=useState<BudgetSnapshot>();
  const [loading,setLoading]=useState(true),[busy,setBusy]=useState(false),[problem,setProblem]=useState<string>();
  const version=useRef(0),mounted=useRef(false),read=useRef<AbortController>(),saving=useRef(false),uncertain=useRef(false);
  const reload=useCallback(()=>{
    read.current?.abort();const controller=new AbortController();read.current=controller;const request=++version.current;
    setLoading(true);setProblem(undefined);
    void readBudgets(controller.signal).then(next=>{if(mounted.current&&request===version.current){setSnapshot(next);uncertain.current=false;}})
      .catch(error=>{if(mounted.current&&request===version.current&&!controller.signal.aborted)setProblem(error instanceof Error?error.message:'Could not load budgets.');})
      .finally(()=>{if(mounted.current&&request===version.current)setLoading(false);});
  },[]);
  useEffect(()=>{mounted.current=true;reload();return()=>{mounted.current=false;++version.current;read.current?.abort();};},[reload]);
  const save=async(values:Readonly<Record<string,number>>)=>{
    if(saving.current||!snapshot||loading)throw new Error('Wait for the budget settings to load.');
    if(uncertain.current)throw new Error('Reload to verify the saved values before saving again.');
    saving.current=true;setBusy(true);setProblem(undefined);const request=++version.current;
    try {const next=await saveBudgets(snapshot.revision,values);if(mounted.current&&request===version.current)setSnapshot(next);}
    catch(error){uncertain.current=true;throw error;}
    finally{saving.current=false;if(mounted.current&&request===version.current)setBusy(false);}
  };
  return <LimitsPanel entries={snapshot?.entries??[]} loading={loading} busy={busy} problem={problem} onSave={save} onReload={reload}/>;
}
