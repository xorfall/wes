import {afterEach,expect,it,vi} from "vitest";
import {LatestDelivery} from "./delivery";
afterEach(()=>vi.useRealTimers());
it("a fast producer neither serializes nor queues every update and only sends the newest snapshot",async()=>{
  vi.useFakeTimers();const send=vi.fn(),fail=vi.fn(),prepare=vi.fn();const delivery=new LatestDelivery(send,fail);
  for(let i=0;i<100000;i++)delivery.update(()=>{prepare();return {input:i};});
  expect(prepare).not.toHaveBeenCalled();await vi.advanceTimersByTimeAsync(100);
  expect(send).toHaveBeenCalledOnce();expect(JSON.parse(send.mock.calls[0]![0]).input).toBe(99999);expect(prepare).toHaveBeenCalledOnce();
  for(let i=100000;i<200000;i++)delivery.update(()=>{prepare();return {input:i};});
  await vi.advanceTimersByTimeAsync(400);expect(send).toHaveBeenCalledOnce();
  delivery.ack(99);await vi.advanceTimersByTimeAsync(100);expect(send).toHaveBeenCalledOnce();
  delivery.ack(1);await vi.advanceTimersByTimeAsync(100);expect(send).toHaveBeenCalledTimes(2);expect(JSON.parse(send.mock.calls[1]![0]).input).toBe(199999);delivery.close();
  await vi.advanceTimersByTimeAsync(10000);expect(fail).not.toHaveBeenCalled();
});
it("slow or oversized views stop their own drawing without accumulating producer work",async()=>{
  vi.useFakeTimers();const send=vi.fn(),fail=vi.fn(),d=new LatestDelivery(send,fail);d.update(()=>({input:1}));await vi.advanceTimersByTimeAsync(5100);
  expect(fail).toHaveBeenCalledWith(expect.stringContaining("5 seconds"),"draw_timeout");d.update(()=>({input:2}));await vi.advanceTimersByTimeAsync(100);expect(send).toHaveBeenCalledOnce();
  const oversized=new LatestDelivery(send,fail);oversized.update(()=>({input:"é".repeat(600000)}));await vi.advanceTimersByTimeAsync(100);expect(fail).toHaveBeenLastCalledWith(expect.stringContaining("1 MiB"),"delivery_failed");expect(send).toHaveBeenCalledOnce();
});
it("should_ReportOnlyActualSendsAndCurrentFlightAcks_When_ObservedWithLabels",async()=>{
  // Arrange
  vi.useFakeTimers();let label="r1";const send=vi.fn(),fail=vi.fn(),sent=vi.fn(),drawn=vi.fn();
  const d=new LatestDelivery(send,fail,100,{label:()=>label,sent,drawn});
  // Act / Assert
  d.update(()=>({input:1}));expect(sent).not.toHaveBeenCalled();
  await vi.advanceTimersByTimeAsync(100);expect(sent).toHaveBeenCalledWith(1,"r1");
  label="r2";d.update(()=>({input:2}));await vi.advanceTimersByTimeAsync(500);
  expect(sent).toHaveBeenCalledOnce();
  d.ack(7);expect(drawn).not.toHaveBeenCalled();
  d.ack(1);expect(drawn).toHaveBeenCalledWith(1,"r1"); // the stored flight label, not the newer one
  d.ack(1);expect(drawn).toHaveBeenCalledOnce();
  await vi.advanceTimersByTimeAsync(100);expect(sent).toHaveBeenLastCalledWith(2,"r2");
  d.close();d.ack(2);expect(drawn).toHaveBeenCalledOnce();
  expect(JSON.parse(send.mock.calls[1]![0])).not.toHaveProperty("label");expect(fail).not.toHaveBeenCalled();
});
it("should_NotReportSent_When_SnapshotExceedsTheBudget",async()=>{
  vi.useFakeTimers();const send=vi.fn(),fail=vi.fn(),sent=vi.fn();
  const d=new LatestDelivery(send,fail,100,{label:()=>"r1",sent,drawn:vi.fn()});
  d.update(()=>({input:"é".repeat(600000)}));await vi.advanceTimersByTimeAsync(100);
  expect(send).not.toHaveBeenCalled();expect(sent).not.toHaveBeenCalled();expect(fail).toHaveBeenCalledWith(expect.any(String),"delivery_failed");
});
