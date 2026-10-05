/**
 * The one way a window takes part in table arrangements: it seeds the store from its settings and
 * says how a change is kept. The session window persists (it owns the preferences); any other
 * window announces the change, and the session's save makes it durable.
 */
import { useEffect, useRef } from "react";
import { tableViewStore, type TableViews } from "../presentation/table-views";

export function useTableViewOwner(views: TableViews, keep: (views: TableViews) => void, owns = true): void {
  const keeper = useRef(keep);
  keeper.current = keep;
  useEffect(() => { if (owns) tableViewStore.seed(views); }, [views, owns]);
  useEffect(() => {
    if (!owns) return;
    tableViewStore.keepWith((next) => keeper.current(next));
    return () => tableViewStore.keepWith(undefined);
  }, [owns]);
}
