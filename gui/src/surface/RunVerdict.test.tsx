import {act,create} from "react-test-renderer";
import {expect,it} from "vitest";
import {Cell,runAvailability} from "./Cell";
import {MonoLine,lineText} from "./MonoLine";
import {RUN_SLOTS,RunSlotValue,runBandOf} from "./RunVerdict";
import type {VerdictField} from "./session-model";

it("uses declared slots even when words or field order resemble another run state",()=>{
  const fields:VerdictField[]=[
    {slot:"retention",segments:[{text:"running"}],keep:false},
    {slot:"state",segments:[{text:"outcome unknown"}],keep:true},
    {segments:[{text:"previous results shown"}],keep:true},
  ];
  const band=runBandOf(fields);
  expect(lineText(band.slots.state)).toBe("outcome unknown");
  expect(lineText(band.slots.duration)).toBe("");
  expect(runAvailability("failed",fields,[])).toMatchObject({said:"outcome unknown",running:false,repeatVerb:"repeat…"});
});

it("keeps all columns mounted across missing, ticking and completed durations",()=>{
  let tree!:ReturnType<typeof create>;
  const draw=(state:string,duration?:string)=>{
    const fields:VerdictField[]=[{slot:"state",segments:[{text:state}],keep:true},
      ...(duration?[{slot:"duration" as const,segments:[{text:duration}],keep:false}]:[])];
    const band=runBandOf(fields);
    const body=<>{RUN_SLOTS.map(slot=><RunSlotValue key={slot} slot={slot} segments={band.slots[slot]}/>)}</>;
    act(()=>{if(tree)tree.update(body);else tree=create(body);});
  };
  draw("running");
  const slots=tree.root.findAllByType(RunSlotValue);
  expect(tree.root.findByProps({className:"cell-slot cell-slot-duration"}).props["aria-hidden"]).toBe("true");
  draw("running","12345678901234567890 min 59 s");
  expect(tree.root.findByProps({className:"cell-slot cell-slot-duration"}).props.title).toBe("12345678901234567890 min 59 s");
  draw("ok","3 ms");
  expect(tree.root.findAllByType(RunSlotValue)).toEqual(slots);
  expect(tree.root.findByProps({className:"cell-slot cell-slot-retention"}).props["aria-hidden"]).toBe("true");
  act(()=>tree.unmount());
});

it("offers known type metadata while the result is unreadable without offering data actions",()=>{
  let tree!:ReturnType<typeof create>;
  act(()=>{tree=create(<Cell theme="keys" state="default" label="synthetic" rows={[]} verdict={[{slot:"state",segments:[{text:"ok"}],keep:true}]}
    blocks={[{key:"n",identity:{id:"n",label:"$result",glyph:"ready"},typeLabel:"List<SyntheticRecord>",hasValue:false,open:true,content:<span>Reading result…</span>}]}
    actions={{open:()=>{throw new Error("Must not read a missing result");}}}/>);});
  expect(tree.root.findByProps({"aria-label":"Type of $result"}).children.join("")).toBe("List<SyntheticRecord>");
  expect(tree.root.findAllByProps({"aria-label":"Data actions"})).toHaveLength(0);
  expect(tree.root.findAllByType(MonoLine).map(line=>lineText(line.props.segments)).join(" ")).not.toContain("List<SyntheticRecord>");
  act(()=>tree.unmount());
});
