import {expect,it,vi} from "vitest";
import {renderedSelectionHost} from "./rendered-selection";
import {InteractionController} from "./interaction";
import {localTimeline} from "./interaction.test-support";
import {SelectionActivations} from "../selection-activations";
const range={start:"2031-01-01T00:00:00Z",end:"2031-01-01T00:10:00Z"};
it("routes a renderer-authored activation with its source version; emit and clear do not run",async()=>{
  const send=vi.fn().mockResolvedValue(undefined),selections=new SelectionActivations(send);
  selections.wait("target","provider read","source",undefined);
  const controller=new InteractionController(localTimeline.interaction!,{range});
  const close=renderedSelectionHost(selections,{node:"source",run:"snapshot"})("",localTimeline,controller);
  controller.emit({kind:"selection",range});await Promise.resolve();expect(send).not.toHaveBeenCalled();
  controller.emit({kind:"selection",range},true);await Promise.resolve();
  expect(send).toHaveBeenCalledOnce();expect(send.mock.calls[0]![2]).toMatchObject({kind:"rendered",source:"source",run:"snapshot",view:"timeline",value:`${range.start}/${range.end}`});
  controller.emit({kind:"selection",range:null},true);await Promise.resolve();expect(send).toHaveBeenCalledOnce();close();
});
it("does not let nested renderers activate the root result, and detaching withdraws queued intent",async()=>{
  const activate=vi.fn(),selections={activate} as unknown as SelectionActivations;
  const controller=new InteractionController(localTimeline.interaction!,{range});
  const attach=renderedSelectionHost(selections,{node:"source",run:"snapshot"});
  attach("nested",localTimeline,controller);controller.emit({kind:"selection",range},true);await Promise.resolve();expect(activate).not.toHaveBeenCalled();
  const close=attach("",localTimeline,controller);controller.emit({kind:"selection",range},true);close();await Promise.resolve();expect(activate).not.toHaveBeenCalled();
});
