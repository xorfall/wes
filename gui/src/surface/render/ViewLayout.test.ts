import { expect,it } from "vitest";
import { DEFAULT_VIEW_LAYOUT,layoutTier } from "./ViewLayout";
it("uses bounded default allocation when dimensions are absent or malformed",()=>{
 expect(layoutTier(undefined,"preview")).toEqual(DEFAULT_VIEW_LAYOUT.preview);
 const bad={...DEFAULT_VIEW_LAYOUT,expanded:{...DEFAULT_VIEW_LAYOUT.expanded,min:{columns:900,rows:0}}};
 expect(layoutTier(bad,"expanded")).toEqual(DEFAULT_VIEW_LAYOUT.expanded);
});
it("honors a renderer's coherent tier independently of the host's width",()=>{
 const layout={...DEFAULT_VIEW_LAYOUT,expanded:{min:{columns:60,rows:12},preferred:{columns:100,rows:16},max:{columns:200,rows:40}}};
 expect(layoutTier(layout,"expanded")).toEqual(layout.expanded);
 expect(layoutTier(layout,"preview")).toEqual(layout.preview);
});
