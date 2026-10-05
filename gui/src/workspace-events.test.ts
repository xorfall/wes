import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { WorkspaceEvents } from "./workspace-events";

class Socket {
  static all: Socket[] = [];
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;
  close = vi.fn();
  send = vi.fn();
  constructor(readonly url: string) { Socket.all.push(this); }
}
beforeEach(() => {
  Socket.all = [];
  vi.useFakeTimers();
  vi.stubGlobal("WebSocket", Socket);
  vi.stubGlobal("window", { location: { href: "http://127.0.0.1:8765/" } });
});
afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });

it("preserves workspace binding and delivers observations without HTTP fetches or writes", () => {
  const fetch = vi.fn(); vi.stubGlobal("fetch", fetch);
  const events = new WorkspaceEvents("/events?workspace=%C3%A7al%C4%B1%C5%9Fma+one");
  const receive = vi.fn(); events.onmessage = receive;
  const socket = Socket.all[0]!;
  expect(socket.url).toBe("ws://127.0.0.1:8765/events/socket?workspace=%C3%A7al%C4%B1%C5%9Fma+one");
  socket.onmessage?.({data:JSON.stringify({sequence:0,events:[{event:'session',generation:'g'}]})});
  expect(receive).toHaveBeenCalledWith({data:'{"event":"session","generation":"g"}'});
  expect(fetch).not.toHaveBeenCalled();
  events.close(); expect(socket.close).toHaveBeenCalledOnce();
});

it("reconnects once per failure, suppresses old frames and cancels scheduled reconnect on close", async () => {
  const events = new WorkspaceEvents("/events");
  const error = vi.fn(); const receive = vi.fn(); events.onerror = error; events.onmessage = receive;
  const first = Socket.all[0]!;
  const stale = first.onmessage!; const closed = first.onclose!;
  first.onerror?.(); closed();
  expect(error).toHaveBeenCalledOnce();
  await vi.advanceTimersByTimeAsync(500);
  expect(Socket.all).toHaveLength(2);
  stale({data:"old"}); expect(receive).not.toHaveBeenCalled();
  Socket.all[1]!.onmessage?.({data:JSON.stringify({sequence:0,events:["fresh"]})});
  expect(receive).toHaveBeenCalledWith({data:'"fresh"'});
  Socket.all[1]!.onclose?.(); events.close();
  await vi.advanceTimersByTimeAsync(10000);
  expect(Socket.all).toHaveLength(2);
});

it("uses bounded backoff for unavailable sockets without replaying commands", async () => {
  const events = new WorkspaceEvents("/events");
  for (const delay of [500, 1000, 2000, 4000, 5000, 5000]) {
    const count = Socket.all.length;
    Socket.all.at(-1)!.onerror?.();
    await vi.advanceTimersByTimeAsync(delay - 1);
    expect(Socket.all).toHaveLength(count);
    await vi.advanceTimersByTimeAsync(1);
    expect(Socket.all).toHaveLength(count + 1);
  }
  events.close();
});

it("does not restart if the error handler retires the owner", async () => {
  const events = new WorkspaceEvents("/events");
  events.onerror = () => events.close();
  Socket.all[0]!.onerror?.();
  await vi.advanceTimersByTimeAsync(10000);
  expect(Socket.all).toHaveLength(1);
});

it("acknowledges a coherent batch only after every event was consumed", () => {
  const events = new WorkspaceEvents("/events"); const socket = Socket.all[0]!;
  const seen: string[] = [];
  events.onmessage = event => { expect(socket.send).not.toHaveBeenCalled(); seen.push(JSON.parse(event.data).event); };
  socket.onmessage?.({ data: JSON.stringify({ sequence: 7, events: [{ event: "session" }, { event: "log-delta", reset: true, entries: [], removed: [] }] }) });
  expect(seen).toEqual(["session", "log-delta"]); expect(socket.send).toHaveBeenCalledWith("ack:7"); events.close();
});


it("retains exact numeric values through observation batch delivery and acknowledgement", () => {
  const events = new WorkspaceEvents("/events"), socket = Socket.all[0]!;
  const received = vi.fn(); events.onmessage = received;
  socket.onmessage?.({data:'{"sequence":1,"events":[{"event":"sample","value":9223372036854775807,"decimal":0.10000000000000000000001}]}'});
  expect(received).toHaveBeenCalledWith({data:'{"event":"sample","value":9223372036854775807,"decimal":0.10000000000000000000001}'});
  expect(socket.send).toHaveBeenCalledWith("ack:1");
  events.close();
});
