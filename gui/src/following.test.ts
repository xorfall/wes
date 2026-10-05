import { describe, expect, it } from "vitest";
import { shouldFollow } from "./following";

/** 500 tall content, 200 visible: the bottom is scrollTop 300. */
const bottom = 300;
const visible = 200;
const before = 500;

describe("staying with the answer", () => {
  it("should_Follow_When_TheViewWasAtTheBottom", () => {
    expect(shouldFollow(false, bottom, visible, before)).toBe(true);
  });

  /** Rounding and a border must not read as somebody having scrolled away. */
  it("should_Follow_When_TheViewWasNearlyAtTheBottom", () => {
    expect(shouldFollow(false, bottom - 30, visible, before)).toBe(true);
  });

  /** Yanking somebody back while they read is worse than never following at all. */
  it("should_StayPut_When_SomebodyScrolledUpToRead", () => {
    expect(shouldFollow(false, 0, visible, before)).toBe(false);
    expect(shouldFollow(false, bottom - 200, visible, before)).toBe(false);
  });

  /** You asked for this answer, so you are following it again wherever you were. */
  it("should_Follow_When_SomethingAskedToBe", () => {
    expect(shouldFollow(true, 0, visible, before)).toBe(true);
  });

  /**
   * The question is asked of the height before the growth. Measuring against the new height would say
   * "far from the bottom" for exactly the growth that should have been followed.
   */
  it("should_Follow_When_TheContentJustGrewUnderneath", () => {
    const grown = 900;

    expect(shouldFollow(false, bottom, visible, before)).toBe(true);
    expect(shouldFollow(false, bottom, visible, grown)).toBe(false);
  });

  it("should_Follow_When_ThereWasNothingBefore", () => {
    expect(shouldFollow(false, 0, visible, 0)).toBe(true);
  });
});
