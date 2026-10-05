export interface ViewAsset {digest:string;definition:import("../definition").ViewDefinition;javascript:string;css:string}
import {frameBaseStyles,roleStyles} from "@wes/view-sdk/theme";
/** Hash-authorized inline code; no nonce that author code could reuse for an external script. */
export async function frameDocument(asset:ViewAsset,theme:string,fonts=""):Promise<string>{
  const bytes=new Uint8Array(await crypto.subtle.digest("SHA-256",new TextEncoder().encode(asset.javascript)));
  const hash=btoa(String.fromCharCode(...bytes));
  // Escaping raw-text terminators changes executable bytes. Reject instead of mismatching the CSP hash.
  if(/<\/script/i.test(asset.javascript)||[asset.css,theme,fonts].some(css=>/<\/style/i.test(css)))throw new Error("View asset contains an unsafe HTML raw-text terminator");
  // Paint the iframe canvas explicitly: WebKit otherwise leaves a white viewport
  // behind transparent renderer bodies, even when the client uses a dark palette.
  const embeddedGround="html{background:var(--surface)}body{background:transparent}";
  return `<!doctype html><html><head><meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'sha256-${hash}'; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; connect-src 'none'; frame-src 'none'; object-src 'none'; form-action 'none'; base-uri 'none'"><style>${fonts}\nhtml,body{margin:0;padding:0;min-width:0;overflow-x:hidden}*{box-sizing:border-box}#root{min-width:0;display:flow-root}\n${frameBaseStyles}\n${embeddedGround}\n${roleStyles()}\n${asset.css}</style><style id="wes-view-theme">${theme}</style></head><body><div id="root"></div><script>${asset.javascript}</script></body></html>`;
}
