import registry from "./theme.json";
import authoring from "./authoring.json";

/** Shared by Surface, the frame host and authoring discovery. Values belong to the client. */
export const viewTheme = registry;
const declarations = (style: Readonly<Record<string,string>>) =>
  Object.entries(style).map(([name,value]) => `${name}:${value}`).join(";");

export function roleStyles(root = ""): string {
  return Object.entries(registry.roles).map(([name,role]) => {
    const selector = root ? `${root} .${name},${root}.${name}` : `.${name}`;
    return `${selector}{${declarations(role.style)}}`;
  }).join("\n");
}

export const frameBaseStyles = Object.entries(registry.baseStyles)
  .map(([selector,style]) => `${selector}{${declarations(style)}}`).join("\n");

/** Derive the bridge's variable allowlist from supported layout tokens and role bindings. */
export const themeTokenNames: readonly string[] = [...new Set([
  ...Object.keys(registry.tokens),
  ...[...Object.values(registry.roles).map(role => role.style),...Object.values(registry.baseStyles)]
    .flatMap(style => Object.values(style).flatMap(value => [...value.matchAll(/var\((--[\w-]+)\)/g)].map(match => match[1]!))),
])].sort();

export function themeVariables(read: (name:string) => string): string {
  return `:root{${themeTokenNames.map(name => `${name}:${read(name).trim() || "initial"}`).join(";")}}`;
}

/** Apply styling alone; never paint, reduce events, replace the root or change revisions. */
export function applyFrameTheme(document: Document, css: unknown): void {
  if (typeof css !== "string" || css.length > authoring.runtimeLimits.messageCharacters || /<\/style/i.test(css)) throw new Error("Invalid View theme");
  const style = document.getElementById("wes-view-theme");
  if (!style || style.tagName !== "STYLE") throw new Error("Missing View theme stylesheet");
  style.textContent = css;
}
