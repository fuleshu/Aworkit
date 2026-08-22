/** Maximum JSON body size accepted by every Aworkit local process boundary. */
export const MAX_FRAME_BYTES = 1024 * 1024;
const PREFIX_BYTES = 4;
const strictUtf8 = new TextDecoder("utf-8", { fatal: true });

/** Encodes one JSON value using the Rust-compatible u32 big-endian prefix. */
export function encodeFrame(value: unknown): Uint8Array {
  const json = JSON.stringify(value);
  if (json === undefined) throw new Error("value has no JSON representation");
  const body = new TextEncoder().encode(json);
  if (body.byteLength > MAX_FRAME_BYTES)
    throw new Error("frame body exceeds limit");
  const frame = new Uint8Array(PREFIX_BYTES + body.byteLength);
  new DataView(frame.buffer).setUint32(0, body.byteLength, false);
  frame.set(body, PREFIX_BYTES);
  return frame;
}

/** Decodes exactly one framed JSON value and rejects malformed or extra input. */
export function decodeFrame(frame: Uint8Array): unknown {
  if (frame.byteLength < PREFIX_BYTES) throw new Error("frame is truncated");
  const bodyLength = new DataView(
    frame.buffer,
    frame.byteOffset,
    PREFIX_BYTES,
  ).getUint32(0, false);
  if (bodyLength > MAX_FRAME_BYTES) throw new Error("frame body exceeds limit");
  if (frame.byteLength !== PREFIX_BYTES + bodyLength)
    throw new Error("frame length is invalid");
  return JSON.parse(strictUtf8.decode(frame.subarray(PREFIX_BYTES)));
}

/** Bounded incremental decoder for split and coalesced local-IPC chunks. */
export class FrameDecoder {
  readonly #buffer = new Uint8Array(PREFIX_BYTES + MAX_FRAME_BYTES);
  #length = 0;
  #frameLength: number | undefined;

  push(chunk: Uint8Array): unknown[] {
    const values: unknown[] = [];
    let offset = 0;
    while (offset < chunk.byteLength) {
      if (this.#length < PREFIX_BYTES) {
        const count = Math.min(
          PREFIX_BYTES - this.#length,
          chunk.byteLength - offset,
        );
        this.#buffer.set(chunk.subarray(offset, offset + count), this.#length);
        this.#length += count;
        offset += count;
        if (this.#length < PREFIX_BYTES) continue;
        const bodyLength = new DataView(
          this.#buffer.buffer,
          0,
          PREFIX_BYTES,
        ).getUint32(0, false);
        if (bodyLength > MAX_FRAME_BYTES) {
          this.reset();
          throw new Error("frame body exceeds limit");
        }
        this.#frameLength = PREFIX_BYTES + bodyLength;
      }

      const expected = this.#frameLength;
      if (expected === undefined)
        throw new Error("frame decoder invariant failed");
      const count = Math.min(
        expected - this.#length,
        chunk.byteLength - offset,
      );
      this.#buffer.set(chunk.subarray(offset, offset + count), this.#length);
      this.#length += count;
      offset += count;
      if (this.#length !== expected) continue;

      const frame = this.#buffer.slice(0, expected);
      this.reset();
      values.push(decodeFrame(frame));
    }
    return values;
  }

  reset(): void {
    this.#length = 0;
    this.#frameLength = undefined;
  }
}
