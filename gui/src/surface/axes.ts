/**
 * The surface's two axes.
 *
 * Paper is the base palette, ink the dark end of it and white a plain one; normal is the base
 * density and dense the other. They are attributes on the surface root rather than more themes, so
 * `ink+dense` needs no third stylesheet: the two blocks in tokens.css simply both apply.
 */
export type Palette = "paper" | "ink" | "white";
export type Density = "normal" | "dense";

export interface Axes {
  readonly palette: Palette;
  readonly density: Density;
}

export const defaultAxes: Axes = { palette: "paper", density: "normal" };
