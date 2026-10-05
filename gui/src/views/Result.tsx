import { useEffect, useState } from "react";
import type { Engine } from "../engine";
import type { StoredValue } from "../protocol";
import { httpTrace, HttpInspection } from "./HttpInspection";
import { ValueBlock } from "../surface/render/ValueBlock";

/**
 * The existing typed result rendering, reusable outside a cell.
 *
 * A panel showing a result must show the same thing a cell shows — the same view choice, the same
 * chart dispatch, the same refusal to pretend a private or missing value is readable. This wraps
 * that dispatch around one fetched value so panels never reimplement it.
 */
export function ValueView({ value, cacheKey, engine }: { readonly value: StoredValue; readonly cacheKey?: string; readonly engine?: Engine }) {
  if (httpTrace(value.data)) return <HttpInspection data={value.data} />;
  return <ValueBlock engine={engine} value={value} cacheKey={cacheKey ?? `view:${keyOf(value)}`} mode="window" inCell={false} />;
}

/** Identity for values drawn outside a cell, where no handle names them. */
const keys = new WeakMap<object, string>();
let next = 0;
function keyOf(value: StoredValue): string {
  let key = keys.get(value);
  if (!key) keys.set(value, key = String(next += 1));
  return key;
}

/**
 * One node's current result, fetched and rendered. A stale fetch never lands: the handle is
 * checked when the reply arrives, so a newer run's value is never overwritten by an older read.
 */
export function NodeResult({
  engine,
  handle,
  isPrivate,
}: {
  readonly engine: Engine;
  readonly handle: string | undefined;
  readonly isPrivate?: boolean;
}) {
  const [value, setValue] = useState<StoredValue>();
  const [trouble, setTrouble] = useState<string>();
  useEffect(() => {
    setValue(undefined);
    setTrouble(undefined);
    if (handle === undefined || isPrivate) return;
    let current = true;
    engine
      .fetch(handle)
      .then((fetched) => { if (current) setValue(fetched); })
      .catch((failure: Error) => { if (current) setTrouble(failure.message); });
    return () => {
      current = false;
    };
  }, [engine, handle, isPrivate]);
  if (isPrivate) return <p className="cell-quiet">Private result &middot; inspect it in the workspace.</p>;
  if (handle === undefined) return <p className="cell-quiet">No result yet.</p>;
  if (trouble !== undefined) return <p className="failure" role="alert">{trouble}</p>;
  if (value === undefined) return <p className="loading-result" role="status">Loading result&hellip;</p>;
  return <ValueView engine={engine} value={value} cacheKey={`node:${handle}`} />;
}
