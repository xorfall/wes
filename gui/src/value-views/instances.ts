import { budget } from "../limits/policy";
import type { StoredValue } from "../protocol";
import { max_active_view_roots, max_view_frame_instances } from "./limits";

export type OutputPort = "data" | "error" | "cancel";
/** Where captured bytes came from: audit only, never authority to read or rerun the source. */
export interface ReferenceOrigin { readonly node: string; readonly port: OutputPort; readonly fields: readonly string[]; readonly run: string | null }
/**
 * What the view's input data is. Current follows committed results of a work output and never runs
 * its producer; Retained is an acknowledged immutable result and never observes; Unlinked is a literal.
 */
export type InputReference =
  | { readonly kind: "unlinked" }
  | { readonly kind: "current"; readonly node: string; readonly port: OutputPort; readonly fields: readonly string[]; readonly shownRun: string | null }
  | { readonly kind: "retained"; readonly node: string; readonly run: string; readonly handle: string; readonly origin: ReferenceOrigin | null };
export interface ViewInstance {
  readonly id: string; readonly instance: string; readonly revision: string; readonly definition: string; readonly digest: string;
  readonly artifact?:string|null;
  readonly inputReference: InputReference;
  /** Only the read budget: a finite result or a bounded stream window. Not part of the reference. */
  readonly inputDelivery: "finite" | "window";
  readonly observing:boolean; readonly inputRevision:string; readonly inputProblem:string|null;readonly inputCautions:readonly string[];
  readonly query: {readonly environment:string|null;readonly template:string;mode:"finite"|"live";adapter:string|null;readonly source:string;readonly output:string;readonly trigger:"manual"|"commit";readonly running:boolean}|null;
  readonly linkedInputs: readonly string[];
  readonly input: StoredValue | null; readonly members: Readonly<Record<string, readonly string[]>>;
}
export interface InputPatches {
  readonly cautions:Readonly<Record<string,readonly string[]>>;readonly values:Readonly<Record<string,Readonly<Record<string,StoredValue>>>>;readonly problems:Readonly<Record<string,string>>}
export interface ViewFrame { readonly root: string; readonly instances: readonly ViewInstance[] }
export interface FrameSample<T=ViewFrame> { readonly frame?: T; readonly problem?: string; readonly paused?: boolean }
interface Watch<T> {
  node: string; generation: string; listeners: Set<(sample: FrameSample<T>) => void>;
  sample: FrameSample<T>; dirty: boolean; retries:number; failed?:boolean; poll?:boolean; etag?: string; controller?: AbortController;
}
/** Finite view edits invalidate snapshots. No renderer polls and no source event queue is retained. */
export class ViewFrameReader<T=ViewFrame> {
  private watches = new Map<string, Watch<T>>();
  private timer?: ReturnType<typeof setTimeout>;
  private active = 0;
  constructor(private read: (node:string, generation:string, signal:AbortSignal, etag?:string, previous?:T) => Promise<{frame:T;etag:string}|{retry:true}|undefined>, private readonly period?:number,private readonly follows?:(frame:T)=>boolean,private readonly capacity=max_active_view_roots()) {}
  watch(node:string, generation:string, listener:(sample:FrameSample<T>)=>void):()=>void {
    const key=`${generation}:${node}`;
    let watch=this.watches.get(key);
    if (!watch) {
      if(this.watches.size>=this.capacity){listener({problem:"Too many visible view subscriptions. Close another view, then reopen this view."});return ()=>{};}
      watch={node,generation,listeners:new Set(),sample:{},dirty:true,retries:0};this.watches.set(key,watch);
    }
    if(watch.failed){watch.failed=false;watch.retries=0;watch.dirty=true;watch.sample={};}
    watch.listeners.add(listener);listener(watch.sample);this.schedule();
    const selected=watch;
    return ()=>{selected.listeners.delete(listener);if(!selected.listeners.size){selected.controller?.abort();this.watches.delete(key);}if(!this.watches.size){clearTimeout(this.timer);this.timer=undefined;}};
  }
  invalidate(){for(const watch of this.watches.values())watch.dirty=true;this.schedule();}
  private schedule(){if(this.timer!==undefined || ![...this.watches.values()].some(w=>!w.failed&&(this.period||w.dirty||w.poll)&&!w.controller))return;
    this.timer=setTimeout(()=>{this.timer=undefined;for(const w of this.watches.values())if(!w.failed&&(this.period||w.poll))w.dirty=true;this.tick();},this.period??([...this.watches.values()].some(w=>w.poll)?250:100));}
  private tick(){
    for(const [key,watch] of [...this.watches]){
      if(this.active>=budget("ui.result.reads"))break;
      if(watch.failed||!watch.dirty||watch.controller)continue;
      this.watches.delete(key);this.watches.set(key,watch);
      watch.dirty=false;const controller=new AbortController();watch.controller=controller;this.active++;
      const deadline=setTimeout(()=>controller.abort(),2000);
      void this.read(watch.node,watch.generation,controller.signal,watch.etag,watch.sample.frame).then(result=>{
        if(this.watches.get(key)!==watch||controller.signal.aborted)return;
        if(result && "retry" in result){
          if(++watch.retries>8)throw new Error("View is busy; reopen to retry. No command was rerun.");
          watch.dirty=true;return;
        }
        watch.retries=0;
        if(result){watch.poll=this.follows?.(result.frame);watch.etag=result.etag;watch.sample={frame:result.frame};for(const listener of watch.listeners)listener(watch.sample);}
      }).catch(error=>{
        if(this.watches.get(key)!==watch)return;
        watch.failed=true;watch.poll=false;watch.etag=undefined;watch.sample={problem:controller.signal.aborted?"View read exceeded its 2-second budget. Reopen to retry.":error instanceof Error?error.message:"View unavailable"};
        for(const listener of watch.listeners)listener(watch.sample);
      }).finally(()=>{clearTimeout(deadline);watch.controller=undefined;this.active--;this.schedule();});
    }
  }
}

/** Deltas may only reuse an exact instance/config/input revision from this reader's prior frame. */
export function mergeViewFrame(raw:ViewFrame,previous?:ViewFrame):ViewFrame {
  if(!Array.isArray(raw.instances)||raw.instances.length>max_view_frame_instances()||new Set(raw.instances.map(i=>i.id)).size!==raw.instances.length)throw new Error("Invalid view frame");
  const prior=new Map(previous?.instances.map(i=>[i.id,i]));
  return {...raw,instances:raw.instances.map(entry=>{
    if(!(entry as unknown as {unchanged?:boolean}).unchanged)return entry;
    const old=prior.get(entry.id);
    if(!old || old.instance!==entry.instance || old.revision!==entry.revision || old.inputRevision!==entry.inputRevision || old.digest!==entry.digest || old.artifact!==entry.artifact)throw new Error("View delta has no matching base frame");
    return old;
  })};
}
