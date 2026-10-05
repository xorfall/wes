import {act,create} from "react-test-renderer";
import {expect,it} from "vitest";
import group from "../../../views/timeline-group/View";
import {demoRange} from "../../../tools/view-dev/work/timeline";
it("hides and focuses opaque connected slots locally without writing shared navigation",()=>{
 const input={view:"timeline-group" as const,title:"Synthetic comparison",range:demoRange},events:unknown[]=[];
 let tree!:ReturnType<typeof create>;
 act(()=>{tree=create(<group.Component input={input} state={group.initial!(input)} revision={0} emit={event=>events.push(event)} slots={{members:[<div key="a">source A</div>,<div key="b">source B</div>]}} context={{mode:"preview",instance:"group"}}/>);});
 expect(tree.root.findAllByProps({"aria-label":"Shared time navigation"})).toHaveLength(0);
 expect(tree.root.findAllByProps({"aria-label":"Local source visibility"})).toHaveLength(0);
 act(()=>tree.root.findByProps({"aria-label":"Source visibility"}).props.onClick());
 act(()=>tree.root.findByProps({"aria-label":"Show source 1"}).props.onClick());
 const tracks=()=>tree.root.findByProps({"aria-label":"Connected timelines"}).findAllByType("div").filter(node=>node.props.hidden!==undefined);
 expect(tracks().map(node=>node.props.hidden)).toEqual([true,false]);
 act(()=>tree.root.findByProps({"aria-label":"Focus source 1"}).props.onClick());
 expect(tracks().map(node=>node.props.hidden)).toEqual([false,true]);
 expect(events).toEqual([]);act(()=>tree.unmount());
});
