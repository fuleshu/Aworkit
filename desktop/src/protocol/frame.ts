/** Maximum JSON payload size accepted by every Aworkit local process boundary. */
export const MAX_FRAME_BYTES = 1024 * 1024;

/** Encodes one JSON value using the Rust-compatible u32 big-endian prefix. */
export function encodeFrame(value: unknown): Uint8Array {
  const body = new TextEncoder().encode(JSON.stringify(value));
  if (body.byteLength > MAX_FRAME_BYTES) {
    throw new Error("frame body exceeds limit");
  }
  const frame = new Uint8Array(4 + body.byteLength);
  new DataView(frame.buffer).setUint32(0, body.byteLength, false);
  frame.set(body, 4);
  return frame;
}

/** Decodes exactly one framed JSON value and rejects malformed or extra input. */
export function decodeFrame(frame: Uint8Array): unknown {
  if (frame.byteLength < 4) throw new Error("frame is truncated");
  const bodyLength = new DataView(frame.buffer, frame.byteOffset, 4).getUint32(0, false);
  if (bodyLength > MAX_FRAME_BYTES) throw new Error("frame body exceeds limit");
  if (frame.byteLength !== 4 + bodyLength) throw new Error("frame length is invalid");
  return JSON.parse(new TextDecoder().decode(frame.subarray(4)));
}
