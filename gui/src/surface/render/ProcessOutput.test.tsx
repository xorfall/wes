import { afterEach,expect,it } from "vitest";
import { act,create,type ReactTestRenderer } from "react-test-renderer";
import type { StoredValue } from "../../protocol";
import { ProcessOutput,previewLines } from "./ProcessOutput";
const value=(stdout:string,stderr="",exitCode=0):StoredValue=>({type:{kind:"record",name:"ProcessOutput",fields:[]},provenance:{},data:{stdout,stderr,exitCode}});
const trees:ReactTestRenderer[]=[];
afterEach(()=>act(()=>trees.splice(0).forEach(tree=>tree.unmount())));
function draw(stored:StoredValue,mode="window"){let tree!:ReactTestRenderer;act(()=>{tree=create(<ProcessOutput value={stored} mode={mode}/>);});trees.push(tree);return tree;}
const said=(tree:ReactTestRenderer)=>tree.root.findAllByType("span").map(span=>span.children.filter(child=>typeof child==="string").join("")).join(" ");
it("bounds wrapping by display rows and reports continued logical lines",()=>{
 const preview=previewLines(["東".repeat(100),"second"],20,2);
 expect(preview.lines).toHaveLength(1);expect(preview.cut).toBe(true);expect(preview.remaining).toBe(1);expect(preview.lines[0]!.text.endsWith("…")).toBe(true);
});
it("decodes CRLF and bare CR independently while keeping nonzero exit a data fact",()=>{
 const tree=draw(value(btoa("stdout\r\nsecond\rthird\n"),btoa("diagnostic\n"),1));
 expect(said(tree)).toContain("exit 1");expect(said(tree)).toContain("diagnostic");
 act(()=>tree.root.findAllByProps({role:"tab"}).find(tab=>tab.children.join("").startsWith("stdout"))!.props.onClick());
 for(const text of ["stdout","second","third"])expect(said(tree)).toContain(text);
});
it("finds line 512 beyond the initial page and keeps find text per stream",()=>{
 const text=Array.from({length:600},(_,at)=>at===511 ? "target-line-512" : `line-${at+1}`).join("\n");
 const tree=draw(value(btoa(text),btoa("stderr-only")));
 expect(said(tree)).not.toContain("target-line-512");
 act(()=>tree.root.findByProps({"aria-label":"Find in stdout"}).props.onChange({target:{value:"target-line-512"}}));
 act(()=>tree.root.findByProps({"aria-label":"Next match"}).props.onClick());
 expect(said(tree)).toContain("target-line-512");
 act(()=>tree.root.findAllByProps({role:"tab"}).find(tab=>tab.children.join("").startsWith("stderr"))!.props.onClick());
 expect(tree.root.findByProps({"aria-label":"Find in stderr"}).props.value).toBe("");
 act(()=>tree.root.findAllByProps({role:"tab"}).find(tab=>tab.children.join("").startsWith("stdout"))!.props.onClick());
 expect(tree.root.findByProps({"aria-label":"Find in stdout"}).props.value).toBe("target-line-512");
});
it("leaves Enter to an input method composing the find text, then steps matches",()=>{
 const text=Array.from({length:600},(_,at)=>at===511 ? "target-line-512" : `line-${at+1}`).join("\n");
 const tree=draw(value(btoa(text)));
 const find=()=>tree.root.findByProps({"aria-label":"Find in stdout"});
 act(()=>find().props.onChange({target:{value:"target-line-512"}}));
 let prevented=0;const enter=(extra:object)=>act(()=>find().props.onKeyDown({key:"Enter",shiftKey:false,preventDefault(){prevented++;},stopPropagation(){},...extra}));
 enter({nativeEvent:{isComposing:true}});enter({keyCode:229});
 expect([prevented,said(tree).includes("target-line-512")]).toEqual([0,false]);
 enter({nativeEvent:{isComposing:false}});
 expect([prevented,said(tree).includes("target-line-512")]).toEqual([1,true]);
});
it("shows malformed Bytes as a read problem and can switch binary streams to hex safely",()=>{
 const invalid=draw(value("%%%"));expect(JSON.stringify(invalid.toJSON())).toContain("invalid base64");
 const binary=draw(value(btoa(String.fromCharCode(0,255,65))));expect(JSON.stringify(binary.toJSON())).toContain("00 ff 41");
});
