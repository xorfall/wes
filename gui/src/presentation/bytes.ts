import { max_decoded_body } from "../views/http-response";

/** A reading of stored Bytes. The stored base64 is kept beside it, never replaced. */
export class DecodedBytes {
  constructor(
    readonly stored: string,
    readonly size: number,
    readonly text: string | undefined,
    readonly problem: string | undefined,
    readonly encoding: string | undefined,
    /** A JSON body parsed from `text`, when the content type said JSON and it parsed. */
    readonly json: { readonly value: unknown } | undefined = undefined,
    /** A body still being decoded (gzip/deflate in flight). */
    readonly pending = false,
  ) {}
}

/** Size of base64 data in bytes, without decoding it. */
export function byteSize(base64: string): number {
  const padding = base64.endsWith("==") ? 2 : base64.endsWith("=") ? 1 : 0;
  return Math.max(0, Math.floor((base64.length * 3) / 4) - padding);
}

/**
 * The text of base64 Bytes, or undefined when they are not text — an image is not a failure.
 * Valid UTF-8 can still be binary, so control bytes other than tab and newline mean "not text".
 */
export function decodeBytes(base64: string): string | undefined {
  try {
    const binary = atob(base64);
    const bytes = Uint8Array.from(binary, (character) => character.charCodeAt(0));
    const text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    return /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/.test(text) ? undefined : text;
  } catch {
    return undefined;
  }
}

/** Plain Bytes (not an HTTP body): utf-8 or not text. Synchronous and bounded. */
export function readBytes(base64: string): DecodedBytes {
  // Reject malformed storage before presenting an estimated size as a byte fact.
  if (base64.length % 4 !== 0 || !/^[A-Za-z0-9+/]*={0,2}$/.test(base64)) {
    return new DecodedBytes(base64, 0, undefined, "invalid base64", undefined);
  }
  const size = byteSize(base64);
  if (size > max_decoded_body()) return new DecodedBytes(base64, size, undefined, "larger than the 16 MiB decoding limit", undefined);
  const text = decodeBytes(base64);
  return text === undefined
    ? new DecodedBytes(base64, size, undefined, "not text", undefined)
    : new DecodedBytes(base64, size, text, undefined, "utf-8");
}
