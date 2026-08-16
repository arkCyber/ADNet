// webtransport.js — minimal WebTransport (HTTP/3) shim that
// mirrors the `a3net-webtransport` runtime on the browser side.
//
// The browser's `WebTransport` API is already standardised; this
// shim only adds the A3Net-specific glue (Noise handshake over the
// first bi-stream, Frame codec on the encrypted channel).
//
// Requires a server that emits a `connect-token` (HMAC-signed) and
// a real TLS cert. For local development you can pass
// `serverCertificateHashes` so the browser skips cert validation
// against the system trust store — useful for self-signed dev certs.

import { ensureSodium, runNoiseHandshake, nodeIdFromRemoteStatic } from "./noise.js";
import { encodeFrame, FrameDecoder } from "./frame.js";

/**
 * Connect to a WebTransport endpoint.
 *
 * @param {string} url - https://host:port/a3net
 * @param {string} token - the connect-token (HMAC-signed claim)
 * @param {object} [opts]
 * @param {Array<{algorithm: string, hash: Uint8Array}>} [opts.serverCertificateHashes]
 * @returns a session with `send`, `next`, `close`, `remoteNodeId`.
 */
export async function connect(url, token, opts = {}) {
  const wt = new WebTransport(url, {
    serverCertificateHashes: opts.serverCertificateHashes,
  });
  await wt.ready;

  // Open the first bidirectional stream. The server uses this for
  // the Noise handshake; after that, additional streams carry
  // encrypted frames.
  const stream = await wt.createBidirectionalStream();
  const writer = stream.writable.getWriter();
  const reader = stream.readable.getReader();

  await ensureSodium();

  // Wire the Noise handshake onto this stream.
  const handshakeBuf = { bytes: new Uint8Array(0), waiter: null };
  const readPump = (async () => {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      const next = new Uint8Array(handshakeBuf.bytes.length + value.length);
      next.set(handshakeBuf.bytes, 0);
      next.set(value, handshakeBuf.bytes.length);
      handshakeBuf.bytes = next;
      if (handshakeBuf.waiter) {
        const w = handshakeBuf.waiter;
        handshakeBuf.waiter = null;
        w();
      }
    }
  })();

  const session = await runNoiseHandshake({
    role: "initiator",
    ioSend: async (msg) => {
      await writer.write(msg);
    },
    ioRecv: async () => {
      while (handshakeBuf.bytes.length < 4) {
        await new Promise((r) => {
          handshakeBuf.waiter = r;
        });
      }
      const len = new DataView(
        handshakeBuf.bytes.buffer,
        handshakeBuf.bytes.byteOffset,
        handshakeBuf.bytes.byteLength,
      ).getUint32(0, false);
      while (handshakeBuf.bytes.length < 4 + len) {
        await new Promise((r) => {
          handshakeBuf.waiter = r;
        });
      }
      const msg = handshakeBuf.bytes.subarray(0, 4 + len);
      handshakeBuf.bytes = handshakeBuf.bytes.subarray(4 + len);
      return msg;
    },
  });

  // After the handshake, additional streams carry A3Net frames.
  // For the demo we just open a second stream and use it for both
  // directions.
  const frameStream = await wt.createBidirectionalStream();
  const frameWriter = frameStream.writable.getWriter();
  const frameReader = frameStream.readable.getReader();
  const decoder = new FrameDecoder();
  const incoming = [];

  (async () => {
    while (true) {
      const { value, done } = await frameReader.read();
      if (done) break;
      let plaintext;
      try {
        plaintext = session.decrypt(value);
      } catch (e) {
        console.warn("decrypt failed:", e);
        continue;
      }
      const frames = decoder.push(plaintext);
      for (const f of frames) incoming.push(f);
    }
  })();

  const nodeId = await nodeIdFromRemoteStatic(session.remoteStaticPub);

  return {
    async send(frame) {
      const encoded = encodeFrame(frame);
      const ct = session.encrypt(encoded);
      await frameWriter.write(ct);
    },
    async next() {
      while (incoming.length === 0) {
        await new Promise((r) => setTimeout(r, 10));
      }
      return { value: incoming.shift(), done: false };
    },
    remoteNodeId() {
      return nodeId;
    },
    async close() {
      try { await frameWriter.close(); } catch (e) {}
      try { wt.close(); } catch (e) {}
    },
  };
}
