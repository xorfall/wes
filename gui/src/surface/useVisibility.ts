import { useEffect, useState, type RefObject } from "react";

/** Visibility controls presentation subscriptions, never source execution. */
export function useVisibility(root: RefObject<HTMLElement>): boolean {
  const observed = typeof IntersectionObserver !== "undefined";
  const [visible, setVisible] = useState(!observed && (typeof document === "undefined" || !document.hidden));
  useEffect(() => {
    const target = root.current;
    if (!target) return;
    const owner = target.ownerDocument;
    let intersects = !observed;
    const update = () => setVisible(intersects && !owner.hidden);
    const observer = observed ? new IntersectionObserver(entries => {
      const entry = entries.filter(entry => entry.target === target).at(-1);
      if (entry) { intersects = entry.isIntersecting; update(); }
    }) : undefined;
    observer?.observe(target);
    owner.addEventListener("visibilitychange", update);
    update();
    return () => { observer?.disconnect(); owner.removeEventListener("visibilitychange", update); };
  }, [root, observed]);
  return visible;
}
