import { describe, expect, it } from "vitest";
import { focusReached, HINT_DELAY_MS, HINT_LINGER_MS, HINT_STAY_MS, HINT_SWITCH_MS, placeHint, pointerMoved, pointerOver } from "./Hints";

const one = { name: "one" };
const other = { name: "other" };

describe("the hint layer", () => {
  it("should_WaitBeforeShowing_When_ThePointerRests", () => {
    // Assert: long enough to read past a hinted element without a hint appearing
    expect(HINT_DELAY_MS).toBeGreaterThanOrEqual(600);
    expect(pointerOver(undefined, one)).toEqual({ kind: "arm", after: HINT_DELAY_MS });
  });

  it("should_StillWait_When_ThePointerMovesFromOneHintedElementToAnother", () => {
    // Arrange: a hint is up; Act: the pointer reaches another hinted element
    const step = pointerOver(one, other);
    // Assert: no instant switch, but a shorter wait than the first
    expect(step).toEqual({ kind: "arm", after: HINT_SWITCH_MS });
    expect(HINT_SWITCH_MS).toBeLessThan(HINT_DELAY_MS);
    expect(HINT_SWITCH_MS).toBeGreaterThan(0);
  });

  it("should_KeepTheHint_When_ThePointerStaysOnItsElement", () => {
    expect(pointerOver(one, one)).toEqual({ kind: "keep" });
  });

  it("should_LetTheHintLingerAndGo_When_ThePointerLeavesForNothingHinted", () => {
    expect(pointerOver(one, undefined)).toEqual({ kind: "hide", after: HINT_LINGER_MS });
    expect(pointerOver(undefined, undefined)).toEqual({ kind: "none" });
  });

  it("should_ShowNothing_When_AClickGaveTheFocus", () => {
    // A hint answers resting, not acting: only keyboard focus (focus-visible) shows one.
    expect(focusReached(false)).toEqual({ kind: "none" });
    expect(focusReached(true)).toEqual({ kind: "show" });
  });

  it("should_LeaveOnItsOwn_When_APointerHintHasStayedLongEnough", () => {
    // Assert: long enough to read twice, short enough to get out of the way of reading
    expect(HINT_STAY_MS).toBeGreaterThanOrEqual(3000);
    expect(HINT_STAY_MS).toBeLessThanOrEqual(6000);
  });

  it("should_Go_When_TheHintedElementWasReplacedUnderThePointer", () => {
    // Arrange: the element the hint belongs to was re-drawn, so no mouseout will ever come for it
    const detached = { isConnected: false, contains: () => true };
    const elsewhere = { isConnected: true, contains: () => false };
    const still = { isConnected: true, contains: () => true };
    // Act / Assert
    expect(pointerMoved(detached, null)).toEqual({ kind: "hide", after: 0 });
    expect(pointerMoved(elsewhere, null)).toEqual({ kind: "hide", after: 0 });
    expect(pointerMoved(still, null)).toEqual({ kind: "keep" });
    expect(pointerMoved(undefined, null)).toEqual({ kind: "none" });
  });

  it("should_SitBelowTheElementsLeftEdge_And_StayInsideTheViewport_When_Placed", () => {
    // Arrange
    const viewport = { width: 1000, height: 600 };
    const hint = { width: 200, height: 30 };
    // Act / Assert
    expect(placeHint({ left: 100, top: 100, bottom: 120 }, hint, viewport)).toEqual({ left: 100, top: 128 });
    expect(placeHint({ left: 950, top: 100, bottom: 120 }, hint, viewport)).toEqual({ left: 788, top: 128 });
    expect(placeHint({ left: 100, top: 570, bottom: 590 }, hint, viewport)).toEqual({ left: 100, top: 532 });
  });
});
