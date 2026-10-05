import { useRef, type ReactNode } from "react";
import type { ViewLayout as Layout } from "../../../../packages/view-sdk/contract";
import type { Mode } from "../../presentation/types";
import { useColumns } from "./measure";

import {layoutTier} from "../../value-views/layout";
export {layoutTier,DEFAULT_VIEW_LAYOUT} from "../../value-views/layout";
/** A renderer declares space, never page scroll or execution authority. */
export function ViewLayout({layout,mode,children,nested=false}:{layout?:Layout|null;mode:Mode;children:ReactNode;nested?:boolean}) {
  const root=useRef<HTMLDivElement>(null),columns=useColumns(root),size=layoutTier(layout,mode);
  return <div ref={root} data-min-rows={size.min.rows} data-max-rows={size.max.rows} data-preferred-rows={size.preferred.rows} data-width-policy={size.placement?.width??"fill"} data-align={size.placement?.align??"start"} className={`view-layout view-layout-${mode}${nested ? " view-layout-nested" : ""}`} style={{["--view-min-columns" as string]:size.min.columns,["--view-min-rows" as string]:size.min.rows,["--view-preferred-columns" as string]:size.preferred.columns,["--view-preferred-rows" as string]:size.preferred.rows,["--view-max-columns" as string]:size.max.columns,["--view-max-rows" as string]:size.max.rows}}>
    {columns<size.min.columns && <p className="mono-faint view-layout-note">needs {size.min.columns} columns · scroll inside the view</p>}
    <div className="view-layout-scroll" tabIndex={0} aria-label="View viewport"><div className="view-layout-content">{children}</div></div>
  </div>;
}
