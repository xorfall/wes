import type { StoredValue } from "./protocol";
import type { Engine } from "./engine";

export interface DisplaySource {
  node: string; run: string; phase: "opening"|"open"|"closing"|"ended"|"stopped"|"failed";
  counts?: { omitted: string; rejected: string; windowItems: number; accepted: string } | null;
}
export interface DisplayMetadata { revision: string; producerRun?: string; epochs: readonly (readonly [string,string])[]; sources: readonly DisplaySource[] }
/** Validate the display boundary before geometry and bigint counters consume it. */
export function validDisplayMetadata(value:unknown):value is DisplayMetadata {
  if(!value || typeof value!=="object")return false;
  const meta=value as DisplayMetadata,integer=(n:unknown)=>typeof n==="string" && /^\d{1,39}$/.test(n);
  return integer(meta.revision) && Array.isArray(meta.epochs) && meta.epochs.length<=512 && meta.epochs.every(pair=>Array.isArray(pair) && pair.length===2 && pair.every(id=>typeof id==="string" && id.length<=256))
    && Array.isArray(meta.sources) && meta.sources.length<=512 && meta.sources.every(source=>source && typeof source.node==="string" && typeof source.run==="string"
      && ["opening","open","closing","ended","stopped","failed"].includes(source.phase)
      && (!source.counts || Number.isSafeInteger(source.counts.windowItems) && source.counts.windowItems>=0 && integer(source.counts.omitted) && integer(source.counts.rejected) && integer(source.counts.accepted)));
}
/**
 * One source's counters in its own scope. `accepted` is in-window plus omitted, where omitted means
 * accepted earlier and since moved out of the rolling window — not lost in transit. Rejected items
 * are never part of `accepted`. No figure is summed across sources or described as saved.
 */
export interface SourceScope { text: string; rejected?: string }
export function sourceScope(counts: NonNullable<DisplaySource["counts"]>): SourceScope {
  const window = `${counts.windowItems} in window`;
  const text = BigInt(counts.omitted) > 0n ? `${counts.accepted} accepted: ${window} + ${counts.omitted} outside window` : `${counts.accepted} accepted · all ${window}`;
  return BigInt(counts.rejected) > 0n ? { text, rejected: `${counts.rejected} rejected` } : { text };
}
export const SOURCE_SCOPE_HELP = "Counts for the current source epoch. Outside window: accepted earlier and no longer in the rolling window, not lost in transit. Rejected items are counted separately and are not accepted. Counts are not saved.";
export interface DisplayRead { value?: StoredValue; metadata: DisplayMetadata }
export class DisplayReadError extends Error {
  constructor(message: string, readonly code: "withdrawn"|"budget"|"busy"|"capacity"|"read", readonly metadata?: DisplayMetadata) { super(message); }
}
export interface LiveSample { value?: StoredValue; revision: number; metadata?: DisplayMetadata; problem?: string; problemCode?: DisplayReadError["code"]; withdrawn?: boolean }
export interface HeldDisplay { sample: LiveSample; at: string }
const readers = new WeakMap<Engine, LiveViewReader>();
export function liveReader(engine: Engine): LiveViewReader {
  let reader = readers.get(engine);
  if (!reader) { reader = new LiveViewReader((node, generation, signal) => engine.liveView(node, generation, signal)); readers.set(engine, reader); }
  return reader;
}
type Watch = { node: string; generation: string; changed: (sample: LiveSample) => void; controller?: AbortController; sample: LiveSample; failed: boolean };
/** Bounded display demand. No event queue, execution, or automatic source replay. */
export class LiveViewReader {
  private watches = new Set<Watch>();
  private held = new Map<string,HeldDisplay>();
  private active = 0;
  private timer?: ReturnType<typeof setTimeout>;
  constructor(private fetch: (node: string, generation: string, signal: AbortSignal) => Promise<DisplayRead | undefined>) {}
  cellSnapshot(node:string,generation:string) { return this.held.get(`${generation}:${node}`); }
  holdCell(node:string,generation:string,snapshot:HeldDisplay|undefined) {
    const key=`${generation}:${node}`;
    if(snapshot) { if(this.held.size<16 || this.held.has(key))this.held.set(key,snapshot); }
    else this.held.delete(key);
  }
  /**
   * Access to `node`'s result was withdrawn: drop its held snapshots in every session and give its
   * watchers an empty, withdrawn sample at once. A read in flight lands nowhere; nothing is read again.
   */
  withdraw(node: string) {
    for (const key of [...this.held.keys()]) if (key.endsWith(`:${node}`)) this.held.delete(key);
    for (const watch of this.watches) {
      if (watch.node !== node) continue;
      watch.controller?.abort();
      watch.failed = true;
      watch.sample = { revision: watch.sample.revision, withdrawn: true, problemCode: "withdrawn", problem: "Result · Access withdrawn" };
      watch.changed(watch.sample);
    }
  }
  watch(node: string, generation: string, changed: Watch["changed"], initial?:LiveSample): () => void {
    if (this.watches.size >= 16) { changed({ revision: 0, problem: "Live view limit reached (16). Close another view before reading this one." }); return () => {}; }
    const watch: Watch = { node, generation, changed, sample:initial??{revision:0}, failed: false };
    this.watches.add(watch); this.schedule();
    return () => { this.watches.delete(watch); watch.controller?.abort(); if (!this.watches.size) { clearTimeout(this.timer); this.timer = undefined; } };
  }
  private schedule() {
    if (this.timer !== undefined || ![...this.watches].some(w => !w.failed)) return;
    this.timer = setTimeout(() => { this.timer = undefined; this.tick(); }, 100);
  }
  private tick() {
    for (const watch of [...this.watches]) {
      if (this.active >= 2) break;
      if (watch.controller || watch.failed) continue;
      this.watches.delete(watch); this.watches.add(watch);
      const controller = new AbortController(); watch.controller = controller; this.active++;
      const deadline = setTimeout(() => controller.abort(), 2000);
      void this.fetch(watch.node, watch.generation, controller.signal).then(read => {
        if (!this.watches.has(watch) || controller.signal.aborted || !read) return;
        const previous=watch.sample.metadata;
        const changedEpoch=previous && JSON.stringify(previous.epochs)!==JSON.stringify(read.metadata.epochs);
        if(changedEpoch)this.holdCell(watch.node,watch.generation,undefined);
        const changedValue=read.value && (changedEpoch || previous?.revision!==read.metadata.revision);
        const changedStatus=JSON.stringify(previous?.sources)!==JSON.stringify(read.metadata.sources);
        if(!changedValue && !changedStatus && previous && !changedEpoch)return;
        watch.sample={value:read.value ?? (changedEpoch ? undefined : watch.sample.value),revision:watch.sample.revision+(changedValue?1:0),metadata:read.metadata};
        watch.changed(watch.sample);
      }).catch(error => {
        // A read aborted by a withdrawal already said so; its abort is not a budget failure.
        if (!this.watches.has(watch) || watch.sample.withdrawn) return;
        // Busy admission is transient; it does not replace or count a sample.
        if(error instanceof DisplayReadError && error.code==="busy")return;
        watch.failed = true;
        const withdrawn=error instanceof DisplayReadError && error.code==="withdrawn";
        const changedEpoch=error instanceof DisplayReadError && error.metadata && watch.sample.metadata && JSON.stringify(error.metadata.epochs)!==JSON.stringify(watch.sample.metadata.epochs);
        if(withdrawn || changedEpoch)this.holdCell(watch.node,watch.generation,undefined);
        watch.sample={...watch.sample,value:withdrawn || changedEpoch?undefined:watch.sample.value,withdrawn,problemCode:error instanceof DisplayReadError ? error.code : "read",metadata:error instanceof DisplayReadError ? error.metadata ?? watch.sample.metadata : watch.sample.metadata,
          problem:controller.signal.aborted ? "Display read exceeded its 2-second budget. Calculation continues; read again to resume." : error instanceof Error ? error.message : "Display read failed."};
        watch.changed(watch.sample);
      }).finally(() => { clearTimeout(deadline); watch.controller = undefined; this.active--; });
    }
    this.schedule();
  }
}
