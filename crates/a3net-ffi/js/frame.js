// frame.js — JS port of `a3net-transport::frame::FrameCodec`.
//
// Wire format (identical on every A3Net transport):
//
//     [4-byte BE length prefix][length bytes of payload]
//
// All four files in `crates/a3net-ffi/js/` (frame.js, noise.js,
// webrtc.js, webtransport.js) live in the browser; they are loaded
// directly from `examples/browser_demo.html` via a <script> tag (or
// via an ES module import if the page is served as such). There is
// no bundler step.
//
// Keep this file in sync with the Rust implementation. Both sides
// must agree on MAX_FRAME_SIZE and the length-prefix format.

const MAX_FRAME_SIZE = 4 * 1024 * 1024; // 4 MiB

/** Encode a single payload as a `Uint8Array` ready to be sent. */
export function encodeFrame(payload) {
  const bytes = payload instanceof Uint8Array ? payload : new TextEncoder().encode(String(payload));
  if (bytes.length > MAX_FRAME_SIZE) {
    throw new Error(`frame too large: ${bytes.length} > ${MAX_FRAME_SIZE}`);
  }
  const out = new Uint8Array(4 + bytes.length);
  new DataView(out.buffer).setUint32(0, bytes.length, false); // big-endian
  out.set(bytes, 4);
  return out;
}

/**
 * Try to decode a single frame from `buf`. Returns `{ frame, consumed }`
 * on success or `null` if `buf` doesn't yet contain a full frame.
 */
export function tryDecode(buf) {
  if (buf.length < 4) return null;
  const len = new DataView(buf.buffer, buf.byteOffset, buf.byteLength).getUint32(0, false);
  if (len > MAX_FRAME_SIZE) {
    throw new Error(`frame too large: ${len} > ${MAX_FRAME_SIZE}`);
  }
  if (buf.length < 4 + len) return null;
  const frame = buf.subarray(4, 4 + len);
  return { frame, consumed: 4 + len };
}

/**
 * Stream-style decoder. Feed it `Uint8Array` chunks; it yields
 * complete frames as they become available. Holds the partial
 * buffer internally.
 */
export class FrameDecoder {
  constructor() {
    this._buf = new Uint8Array(0);
  }
  /** Feed bytes; returns an array of decoded `Uint8Array` frames. */
  push(chunk) {
    if (chunk.length === 0) return [];
    const next = new Uint8Array(this._buf.length + chunk.length);
    next.set(this._buf, 0);
    next.set(chunk, this._buf.length);
    this._buf = next;
    const out = [];
    while (this._buf.length >= 4) {
      const decoded = tryDecode(this._buf);
      if (!decoded) break;
      out.push(decoded.frame);
      this._buf = this._buf.subarray(decoded.consumed);
    }
    return out;
  }
}
