import {themeVariables} from "@wes/view-sdk/theme";

export function currentTheme(element: Element): string {
  const style = getComputedStyle(element);
  // Hidden iframe ancestors may have no resolved tokens in WebKit. The mounted
  // client remains visible while its session is covered by a settings screen.
  const client = element.closest?.(".wes-terminal");
  const fallback = client && client !== element ? getComputedStyle(client) : style;
  return themeVariables(name => style.getPropertyValue(name).trim() || fallback.getPropertyValue(name));
}

/** Settings belong to the mounted client. Observe ancestors, without observing renderer output. */
export function followTheme(element: Element, changed: (css:string)=>void): ()=>void {
  let previous = "", pending: number|undefined;
  const sample = () => {
    pending = undefined;
    const css = currentTheme(element);
    if (css !== previous) { previous = css; changed(css); }
  };
  const observer = new MutationObserver(() => {
    if (pending === undefined) pending = requestAnimationFrame(sample);
  });
  for (let ancestor: Element|null = element; ancestor; ancestor = ancestor.parentElement) {
    observer.observe(ancestor,{attributes:true,attributeFilter:["style","class","hidden","data-palette","data-density"]});
  }
  sample();
  return () => { observer.disconnect(); if (pending !== undefined) cancelAnimationFrame(pending); };
}
