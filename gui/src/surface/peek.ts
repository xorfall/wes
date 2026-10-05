/** The pieces of a cell that open on their own in a plain window: ⌘click on the verdict, the blocks, the source. */
export type PeekWhat = "type" | "value" | "source" | "error";

export const PEEK_WHATS: readonly PeekWhat[] = ["type", "value", "source", "error"];
