import { isExactNumber } from "../exact-json";
/** Small, local view messages. This contract carries no execution capability. */
export interface InteractionProtocol<State, Event> {
  readonly id: string;
  readonly state: (value: unknown) => value is State;
  readonly event: (value: unknown) => value is Event;
}
export interface InteractionPort<State, Event> {
  readonly state: Readonly<State>;
  readonly revision: number;
  /** False means malformed, over budget, rejected by the reducer, or unmounted. */
  emit(event: Event): boolean;
}
export interface InteractionDefinition<State, Event> {
  readonly protocol: InteractionProtocol<State, Event>;
  readonly initial: (model: unknown) => State;
  readonly reduce: (state: Readonly<State>, event: Event) => State;
}
export interface InteractionSnapshot<State> {
  readonly state: Readonly<State>;
  readonly revision: number;
  readonly error?: string;
}

/** Copy before validation/reduction; a module cannot mutate another reader's state.
 * Bounds cover event and state, never the view's source data. */
function message(value: unknown): unknown {
  let left = 16384, nodes = 512;
  const seen = new Set<object>();
  const copy = (input: unknown, depth: number): unknown => {
    if (--nodes < 0 || depth > 12) throw new Error("Message budget");
    if (input === null || typeof input === "boolean") { left -= 4; return input; }
    if (isExactNumber(input)) {left-=input.text.length*2;if(left<0)throw new Error("Message budget");return input;}
    if (typeof input === "number" && Number.isFinite(input)) { left -= 8; return input; }
    if (typeof input === "string") { left -= input.length * 2; if (left < 0) throw new Error("Message budget"); return input; }
    if (typeof input !== "object" || input === null || seen.has(input)) throw new Error("Not a data message");
    seen.add(input);
    if (Array.isArray(input)) {
      if (input.length > nodes) throw new Error("Message budget");
      return Object.freeze(input.map(it => copy(it, depth + 1)));
    }
    if (Object.getPrototypeOf(input) !== Object.prototype && Object.getPrototypeOf(input) !== null) throw new Error("Not a record");
    const result: Record<string, unknown> = Object.create(null);
    for (const [key, descriptor] of Object.entries(Object.getOwnPropertyDescriptors(input))) {
      if (!("value" in descriptor) || !descriptor.enumerable) throw new Error("Not a data field");
      left -= key.length * 2;
      if (left < 0) throw new Error("Message budget");
      result[key] = copy(descriptor.value, depth + 1);
    }
    return Object.freeze(result);
  };
  return copy(value, 0);
}

/** Retain unchanged subtrees so a cursor update does not rebuild plot geometry. */
function share(previous: unknown, next: unknown): unknown {
  if (Object.is(previous, next)) return previous;
  if (typeof previous !== "object" || previous === null || typeof next !== "object" || next === null || Array.isArray(previous) !== Array.isArray(next)) return next;
  const old = previous as Record<string, unknown>, value = next as Record<string, unknown>;
  const keys = Object.keys(value);
  const result: Record<string, unknown> = Array.isArray(next) ? [] : Object.create(null);
  let same = keys.length === Object.keys(old).length;
  for (const key of keys) { result[key] = share(old[key], value[key]); if (!Object.is(result[key], old[key])) same = false; }
  return same ? previous : Object.freeze(result);
}

export interface FrameScheduler { request(callback: () => void): unknown; cancel(handle: unknown): void }
const frames: FrameScheduler = {
  request: callback => typeof requestAnimationFrame === "function" ? requestAnimationFrame(callback) : setTimeout(callback, 16),
  cancel: handle => typeof cancelAnimationFrame === "function" ? cancelAnimationFrame(handle as number) : clearTimeout(handle as ReturnType<typeof setTimeout>),
};

/** One reducer owner per instance (or explicit parent binding), one pending frame, no event queue. */
export class InteractionController<State, Event> {
  private current: InteractionSnapshot<State>;
  private published: InteractionSnapshot<State>;
  private readonly listeners = new Set<() => void>();
  private beforeCommit?: (previous: Readonly<State>, next: Readonly<State>, event:Event) => boolean;
  private pending: unknown;
  private scheduled = false;
  constructor(readonly definition: InteractionDefinition<State, Event>, model: unknown, private readonly scheduler = frames) {
    const state = message(definition.initial(model));
    if (!definition.protocol.state(state)) throw new Error("Invalid initial view state");
    this.current = this.published = Object.freeze({ state, revision: 0 });
  }
  readonly snapshot = () => this.published;
  readonly subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
      if (this.listeners.size === 0 && this.scheduled) {
        this.scheduler.cancel(this.pending); this.scheduled = false;
        this.published = this.current;
      }
    };
  };
  private publish() {
    if (this.scheduled) return;
    if (this.listeners.size === 0) { this.published = this.current; return; }
    this.scheduled = true;
    this.pending = this.scheduler.request(() => {
      this.scheduled = false; this.published = this.current;
      this.listeners.forEach(listener => listener());
    });
  }
  /** Host-owned committed fields; applying remote state never emits a renderer event. */
  adopt(fields: Readonly<Record<string, unknown>>): boolean {
    try {
      const state = share(this.current.state, message({...this.current.state, ...fields}));
      if (!this.definition.protocol.state(state)) throw new Error("Invalid shared state");
      if (state !== this.current.state || this.current.error) {
        this.current = Object.freeze({state, revision:this.current.revision+1}); this.publish();
      }
      return true;
    } catch { this.problem("Shared view state rejected"); return false; }
  }
  committed(): Readonly<State> { return this.current.state; }
  committedRevision():number {return this.current.revision;}
  intercept(commit: (previous: Readonly<State>, next: Readonly<State>, event:Event) => boolean): () => void {
    this.beforeCommit = commit;
    return () => { if(this.beforeCommit === commit)this.beforeCommit = undefined; };
  }
  problem(error: string) {
    this.current = Object.freeze({...this.current,error}); this.publish();
  }
  emit(input: unknown): boolean {
    try {
      const event = message(input);
      if (!this.definition.protocol.event(event)) throw new Error("Invalid event");
      const reduced = this.definition.reduce(this.current.state, event);
      const state = share(this.current.state, message(reduced));
      if (!this.definition.protocol.state(state)) throw new Error("Invalid state");
      if(this.beforeCommit && !this.beforeCommit(this.current.state,state,event))return false;
      if (!this.current.error && state === this.current.state) return true;
      this.current = Object.freeze({ state, revision: this.current.revision + 1 });
      this.publish(); return true;
    } catch {
      // Exceptions may contain source data. Only this bounded, generic diagnostic escapes.
      if (!this.current.error) {
        this.current = Object.freeze({ ...this.current, error: "View interaction rejected" }); this.publish();
      }
      return false;
    }
  }
}
