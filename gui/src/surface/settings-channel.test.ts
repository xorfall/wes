import { afterEach, describe, expect, it, vi } from "vitest";
import { defaults } from "../settings";
import { announceSettings, changed, followSettings } from "./settings-channel";

/* A broadcast channel as a bare fixture: every instance shares one set of listeners. */
function fakeChannel() {
  const listeners = new Set<(event: { data: unknown }) => void>();
  const posted: unknown[] = [];
  class Channel {
    constructor(readonly name: string) {}
    postMessage(data: unknown) { posted.push(data); listeners.forEach((hear) => hear({ data })); }
    addEventListener(_: string, hear: (event: { data: unknown }) => void) { listeners.add(hear); }
    removeEventListener(_: string, hear: (event: { data: unknown }) => void) { listeners.delete(hear); }
    close() {}
  }
  return { Channel, posted, listeners };
}

afterEach(() => vi.unstubAllGlobals());

describe("what a settings window tells the session", () => {
  it("should_NameOnlyTheKeysThatChanged_And_NeverTheLayout", () => {
    // Arrange
    const was = { ...defaults, paneLayout: undefined };
    const next = { ...was, direction: "TB" as const, stayAtNewest: !was.stayAtNewest, paneLayout: { nextId: 9 } as never };
    // Act / Assert
    expect(changed(was, next)).toEqual({ direction: "TB", stayAtNewest: !was.stayAtNewest });
    expect(changed(was, was)).toEqual({});
  });

  it("should_ReachAFollower_When_AChangeIsAnnounced", () => {
    // Arrange
    const { Channel, posted } = fakeChannel();
    vi.stubGlobal("BroadcastChannel", Channel);
    const heard = vi.fn();
    const stop = followSettings(heard);
    // Act
    const said = announceSettings({ direction: "TB" });
    stop();
    announceSettings({ direction: "LR" });
    // Assert
    expect(said).toBe(true);
    expect(heard).toHaveBeenCalledTimes(1);
    expect(heard).toHaveBeenCalledWith({ direction: "TB" });
    expect(posted).toHaveLength(2);
  });

  it("should_SayItWentNowhere_When_ThereIsNoChannel", () => {
    // Arrange
    vi.stubGlobal("BroadcastChannel", undefined);
    // Act / Assert
    expect(announceSettings({ direction: "TB" })).toBe(false);
    expect(announceSettings({})).toBe(true);
    expect(followSettings(() => undefined)).toBeTypeOf("function");
  });
});
