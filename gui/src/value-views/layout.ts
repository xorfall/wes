import type {ViewLayout as Layout,ViewTier} from "../../../packages/view-sdk/contract";
import type {Mode} from "../presentation/types";
const tier=(columns:number,rows:number,preferredRows:number,maxRows:number):ViewTier=>({min:{columns,rows},preferred:{columns:80,rows:preferredRows},max:{columns:320,rows:maxRows}});
export const DEFAULT_VIEW_LAYOUT:Layout={preview:tier(24,4,6,8),expanded:tier(32,6,14,40),window:tier(32,6,24,80)};
export function layoutTier(layout:Layout|null|undefined,mode:Mode):ViewTier {
  const candidate=layout?.[mode];
  if(candidate && ["columns","rows"].every(axis=>{
    const key=axis as "columns"|"rows",bounds=[candidate.min?.[key],candidate.preferred?.[key],candidate.max?.[key]];
    return bounds.every(value=>Number.isInteger(value) && value>0 && value<=(key==="columns"?512:200)) && bounds[0]!<=bounds[1]! && bounds[1]!<=bounds[2]!;
  }) && (!candidate.placement || ['fill','preferred'].includes(candidate.placement.width) && ['start','center','end'].includes(candidate.placement.align)))return candidate;
  return DEFAULT_VIEW_LAYOUT[mode];
}

/** Preview is a renderer-selected summary, not a fixed-height allocation. */
export function frameHeight(layout:Layout|null|undefined,mode:Mode,natural:number,lineHeight:number):number {
  const leading=Number.isFinite(lineHeight)&&lineHeight>0?lineHeight:21;
  const height=Number.isFinite(natural)?Math.max(0,natural):0;
  return Math.min(mode==="preview" ? 20000 : layoutTier(layout,mode).max.rows*leading,height);
}
