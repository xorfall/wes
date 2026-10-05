import { createContext, useContext, useLayoutEffect, useMemo, useSyncExternalStore, type ComponentType } from "react";
import type { ValueViewModule, ViewComponentProps } from "./contract";
import { InteractionController, type InteractionDefinition, type InteractionPort } from "./interaction";

export type InteractiveViewProps<Model, State, Event> = Omit<ViewComponentProps, "model" | "interaction"> & {
  readonly model: Model;
  readonly interaction: InteractionPort<State, Event>;
};

/** Generic erasure happens once at registration. Runtime validators stay at every boundary. */
export function defineInteractiveView<Model, State, Event>(module: Omit<ValueViewModule, "Component" | "interaction"> & {
  readonly interaction: InteractionDefinition<State, Event>;
  readonly Component: ComponentType<InteractiveViewProps<Model, State, Event>>;
}): ValueViewModule {
  const Component = module.Component;
  return { ...module,
    interaction: module.interaction as InteractionDefinition<unknown, unknown>,
    Component: props => {
      if (!props.interaction) throw new Error("Missing interaction host");
      return <Component {...props} model={props.model as Model} interaction={props.interaction as InteractionPort<State, Event>} />;
    },
  };
}

export const InstanceInteractionHost = createContext<((path:string,module:ValueViewModule,controller:SharedInteraction)=>()=>void)|undefined>(undefined);
export type SharedInteraction = InteractionController<unknown, unknown>;
export function InteractiveModule({ module, model, shared, path, render }: {
  module: ValueViewModule; model: unknown; shared?: SharedInteraction; path?:string;
  render: (port: InteractionPort<unknown, unknown>, controller: SharedInteraction) => React.ReactNode;
}) {
  const definition = module.interaction!;
  // Data revision is intentionally absent. A binding change remounts the host above this owner.
  const controller = useMemo(() => shared ?? new InteractionController(definition, model), [definition, shared]);
  const host = useContext(InstanceInteractionHost);
  useLayoutEffect(()=>!shared && path && host ? host(path,module,controller):undefined,[shared,path,host,module,controller]);
  const lease = useMemo(() => ({ active: false }), [controller]);
  useLayoutEffect(() => { lease.active = true; return () => { lease.active = false; }; }, [lease]);
  const snapshot = useSyncExternalStore(controller.subscribe, controller.snapshot, controller.snapshot);
  const port = useMemo(() => ({ state: snapshot.state, revision: snapshot.revision,
    emit: (event: unknown) => lease.active && controller.emit(event),
  }), [snapshot, controller, lease]);
  return <>{snapshot.error && <p className="mono-warn" role="status">{snapshot.error}</p>}{render(port, controller)}</>;
}
