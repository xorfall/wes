import { packagedViews } from "./generated";
import type { StoredValue, TypeShape } from "../protocol";
import type { ValueViewModule } from "./contract";
import { httpViewModule } from "./http/module";

/** Trusted host adapters and checked, isolated package renderers share one registry. */
export function createValueViewRegistry(initial: readonly ValueViewModule[] = []) {
  let modules = [...initial];
  const listeners = new Set<() => void>();
  const publish = () => listeners.forEach(fn => fn());
  return {
    get: () => modules as readonly ValueViewModule[],
    address: (module:ValueViewModule) => {
      const name=module.definition?.id??module.id;
      return modules.find(it=>(it.definition?.id??it.id)===name)===module?name:module.id;
    },
    subscribe(fn: () => void) { listeners.add(fn); return () => { listeners.delete(fn); }; },
    named: (id: string, artifact?:string|null) => modules.find(module => artifact===undefined ? module.id===id || module.definition?.id===id : (module.definition?.id??module.id)===id && (module.definition?.artifact??null)===(artifact??null)),
    find(type: TypeShape, data: unknown, requested?: string, available = modules as readonly ValueViewModule[]) {
      return available.find(module => {
        if (requested !== undefined && module.id !== requested && module.definition?.id !== requested) return false;
        try { return module.matches(type, data); } catch { return false; }
      });
    },
    register(module: ValueViewModule) {
      if (["result", "json", "details", "source", "trace", "chart", "copy", "follow"].includes(module.id) || !/^[a-z][a-z0-9-]*$/.test(module.id) || modules.some(it => it.id === module.id && (it.definition?.artifact??null)===(module.definition?.artifact??null))) {
        throw new Error("View id must be unique and use lowercase words separated by hyphens");
      }
      modules = [...modules, module]; publish();
      return () => { modules = modules.filter(it => it !== module); publish(); };
    },
  };
}

export const valueViewModules = createValueViewRegistry([httpViewModule, ...packagedViews]);

/** Renderer availability belongs to the observed workspace value, not the global code cache. */
const valueModules = new WeakMap<StoredValue, readonly ValueViewModule[]>();
export const builtinValueViews = [httpViewModule, ...packagedViews] as readonly ValueViewModule[];
export function bindValueViews(value: StoredValue, modules: readonly ValueViewModule[]) { valueModules.set(value, modules); }
export function viewsOfValue(value: Pick<StoredValue, "type" | "data">): readonly ValueViewModule[] | undefined {
  return valueModules.get(value as StoredValue);
}
