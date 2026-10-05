import {webcrypto,createHash} from "node:crypto";
import {expect,it,vi} from "vitest";
import {frameDocument,type ViewAsset} from "./document";
import {applyFrameTheme,roleStyles} from "@wes/view-sdk/theme";
it("author code is hash authorized inside an opaque sandbox, without external resources or reusable nonce",async()=>{
  vi.stubGlobal("crypto",webcrypto);const javascript="window.fixture = true;",asset={javascript,css:"body{color:red}"} as ViewAsset;
  try{const doc=await frameDocument(asset,"body{font-family:monospace}");expect(doc).toContain(`'sha256-${createHash('sha256').update(javascript).digest('base64')}'`);expect(doc).toContain("connect-src 'none'");expect(doc).toContain("frame-src 'none'");expect(doc).not.toContain("nonce=");
    await expect(frameDocument({...asset,css:"</style><script>"},"")).rejects.toThrow("terminator");
  }finally{vi.unstubAllGlobals();}
});
it("hosts current shared roles separately from mutable theme variables and retains data-only font policy",async()=>{
  vi.stubGlobal("crypto",webcrypto);
  try{
    const fonts='@font-face{font-family:"Fixture";src:url(data:font/woff2;base64,AA)}';
    const doc=await frameDocument({javascript:"void 0",css:""} as ViewAsset,":root{--surface:white}",fonts);
    expect(doc).toContain(roleStyles());expect(doc).toContain('id="wes-view-theme"');
    expect(doc).toContain(fonts);expect(doc).toContain("font-src data:");expect(doc).not.toContain("font-src http");
    await expect(frameDocument({javascript:"void 0",css:""} as ViewAsset,"","</style>")).rejects.toThrow("terminator");
    const style={tagName:"STYLE",textContent:"old"},document={getElementById:()=>style} as unknown as Document;
    for(const invalid of [null,{},"x".repeat(16385),"</style>"])expect(()=>applyFrameTheme(document,invalid)).toThrow();
    expect(style.textContent).toBe("old");
  }finally{vi.unstubAllGlobals();}
});

it("paints the iframe canvas with the client surface while retaining transparent renderer bodies",async()=>{
  vi.stubGlobal("crypto",webcrypto);
  try{
    for(const surface of ["#F2F3EF","#121A20","#FAFAFA"]){
      const theme=`:root{--surface:${surface}}`;
      for(const css of [".container{padding:8px}",".plot{min-width:0}",".panel{background:var(--surface)}"]){
        const doc=await frameDocument({javascript:"void 0",css} as ViewAsset,theme);
        const body=[...doc.matchAll(/(?:^|[}\n])body\{([^}]+)\}/g)].at(-1)?.[1];
        expect(body).toContain("background:transparent");
        expect(body).not.toContain("background:var(--surface)");
        expect(doc).toContain("html{background:var(--surface)}");
        expect(doc).toContain(theme);expect(doc).toContain(css);
        expect(doc).toContain("frame-src 'none'");
      }
    }
  }finally{vi.unstubAllGlobals();}
});
