/** Finite presentation reads leave the HTTP/1 origin free for shell input and saves.
 * Revisions suppress redundant publication; no dedicated long-lived connection is held.
 */
import { registryStore } from "./registry-store";
import type { HomeFile } from "./registry";

interface Listing {
  readonly revision: string;
  readonly files: readonly HomeFile[];
  readonly problems: readonly { readonly name: string; readonly message: string }[];
}

function isListing(value: unknown): value is Listing {
  const listing = value as Partial<Listing> | null;
  return typeof listing === "object" && listing !== null && typeof listing.revision === "string"
    && Array.isArray(listing.files) && listing.files.every((file) => typeof file?.name === "string" && typeof file?.text === "string")
    && Array.isArray(listing.problems);
}

/** One round: the listing after `revision`, or undefined when the route is absent or failed. */
export async function fetchPresentations(_revision: string | undefined, signal: AbortSignal, fetcher: typeof fetch = fetch): Promise<Listing | undefined> {
  // Do not use ?after: that holds an HTTP/1 connection for up to 25 seconds.
  const url = "/presentations";
  const response = await fetcher(url, { signal: AbortSignal.any([signal, AbortSignal.timeout(5000)]), cache: "no-store" });
  if (response.status === 404 || response.status === 501) return undefined;
  if (!response.ok) throw new Error("Presentation registry is temporarily unavailable");
  const body: unknown = await response.json();
  if (!isListing(body)) throw new Error("Invalid presentation registry listing");
  return body;
}

/** Abortable idle time holds no connection and cannot schedule work after teardown. */
function pauseUntil(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise(resolve => {
    const done = () => { clearTimeout(timer); signal.removeEventListener("abort", done); resolve(); };
    const timer = setTimeout(done, ms);
    signal.addEventListener("abort", done, { once: true });
    if (signal.aborted) done();
  });
}

/** Missing routes stop quietly. Transient failures retain entries and retry after idle time. */
export async function followPresentations(signal: AbortSignal, fetcher: typeof fetch = fetch,
  pause: (ms: number, signal: AbortSignal) => Promise<void> = pauseUntil): Promise<void> {
  let revision: string | undefined;
  while (!signal.aborted) {
    try {
      const listing = await fetchPresentations(revision, signal, fetcher);
      if (signal.aborted || listing === undefined) return;
      if (listing.revision !== revision) registryStore.setHome(listing.files, listing.problems);
      revision = listing.revision;
    } catch {
      if (signal.aborted) return;
    }
    await pause(5000, signal);
  }
}
