/**
 * Whether the view should stay at the bottom while what it shows grows.
 *
 * <p>A terminal follows its output and stops the moment you scroll up to read something. Getting that
 * from scroll events alone does not work: growing the content makes the browser fire a scroll event too,
 * and reading "the bottom is now far away" out of it is indistinguishable from somebody scrolling — so
 * the first answer that arrived switched following off, and the rest of it stayed below the fold.
 *
 * <p>So the question is asked of the height <em>before</em> the growth. Were we at the bottom of what
 * there was? Then follow what there is now. Nothing about it depends on catching an event at the right
 * moment.
 */

/** How far from the bottom still counts as the bottom. Rounding and a border must not read as intent. */
const NEAR = 40;

/**
 * @param pinned       whether something just asked to be followed, such as pressing Enter
 * @param scrollTop    where the view is scrolled to
 * @param clientHeight how much of it is visible
 * @param heightBefore how tall the content was before it grew
 * @return whether to jump to the bottom
 */
export function shouldFollow(
  pinned: boolean,
  scrollTop: number,
  clientHeight: number,
  heightBefore: number,
): boolean {
  return pinned || scrollTop + clientHeight >= heightBefore - NEAR;
}
