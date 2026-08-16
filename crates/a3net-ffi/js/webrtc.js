// webrtc.js — JS shim that wires the browser `RTCPeerConnection`
// to the A3Net Frame + Noise pipelines.
//
// This is the browser-side counterpart of the native
// `a3net_webrtc::dc_session`. The flow is:
//
//   1. Create an `RTCPeerConnection` with the configured ICE servers.
//   2. Open a single ordered+reliable DataChannel labeled `a3net/0`.
//   3. Run the Noise_XX handshake over the DC (via `noise.js`).
//   4. After Noise completes, encrypt A3Net Frames and write them
//      onto the DC, decrypt incoming bytes and yield Frames via the
//      `FrameDecoder` in `frame.js`.
//
// The DC carries **length-prefixed Noise messages during the
// handshake** and then **raw encrypted bytes (no length prefix)
// after**. We switch modes at `handshakeDone()`.

import { ensureSodium, runNoiseHandshake, nodeIdFromRemoteStatic } from "./noise.js";
import { encodeFrame, FrameDecoder } from "./frame.js";

const DC_LABEL = "a3net/0";

/**
 * Open a WebRTC DataChannel session as the *initiator*. Returns
 * an object with `send(frame)` and `events` for incoming frames.
 *
 * @param {RTCPeerConnection} pc - peer connection with the remote
 *   SDP already applied and remote ICE candidates trickled in.
 * @param {object} opts
 * @param {(sdpB64: string) => Promise<string>} opts.sendOffer -
 *   publish the local SDP and receive the answer.
 * @param {(candidateJson: string) => Promise<void>} [opts.sendIce] -
 *   publish a local ICE candidate (best-effort).
 * @param {() => Promise<void>} [opts.shutdown]
 */
export async function openAsInitiator(pc, opts) {
  const dc = pc.createDataChannel(DC_LABEL, { ordered: true });

  // Wait for the DC to open before driving the handshake.
  await waitForOpen(dc);

  return connectNoiseAndFrames(pc, dc, "initiator", opts);
}

/**
 * Open as the *responder*. The remote peer must have called
 * `createDataChannel(DC_LABEL)` first; we wait for the `ondatachannel`
 * event and then proceed.
 */
export async function openAsResponder(pc, opts) {
  const dc = await waitForDataChannel(pc, DC_LABEL);
  await waitForOpen(dc);
  return connectNoiseAndFrames(pc, dc, "responder", opts);
}

async function connectNoiseAndFrames(pc, dc, role, opts) {
  const decoder = new FrameDecoder();
  // Queue of incoming decrypted frames, fed by the onmessage handler.
  const incoming = [];

  const sodium = await ensureSodium();

  // `ioSend`/`ioRecv` use the DC directly, but during the
  // handshake we frame every Noise message with `u32 BE | body`.
  const handshakeIoSend = async (msg) => {
    if (dc.readyState !== "open") {
      throw new Error("data channel not open during handshake");
    }
    dc.send(msg);
  };
  // For the responder the first Noise message comes from the
  // initiator — we buffer incoming bytes until handshake starts.
  const handshakeBuf = { bytes: new Uint8Array(0), waiter: null, done: false };
  dc.addEventListener("message", (ev) => onDcMessage(ev, handshakeBuf, incoming));

  const handshakeIoRecv = async () => {
    while (true) {
      if (handshakeBuf.bytes.length >= 4) {
        const len = new DataView(
          handshakeBuf.bytes.buffer,
          handshakeBuf.bytes.byteOffset,
          handshakeBuf.bytes.byteLength,
        ).getUint32(0, false);
        if (handshakeBuf.bytes.length >= 4 + len) {
          const msg = handshakeBuf.bytes.subarray(0, 4 + len);
          handshakeBuf.bytes = handshakeBuf.bytes.subarray(4 + len);
          return msg;
        }
      }
      // Wait for more data.
      await new Promise((resolve) => {
        handshakeBuf.waiter = resolve;
      });
    }
  };

  let session;
  try {
    session = await runNoiseHandshake({
      role,
      ioSend: handshakeIoSend,
      ioRecv: handshakeIoRecv,
    });
  } catch (e) {
    throw new Error(`Noise handshake failed: ${e}`);
  }

  // Handshake complete. The DC continues to deliver raw bytes;
  // we now buffer encrypted ciphertext into `incoming` and the
  // application reads via `await next()`.
  dc.removeEventListener("message", (ev) => onDcMessage(ev, handshakeBuf, incoming));
  dc.addEventListener("message", (ev) => {
    const data = ev.data instanceof ArrayBuffer ? new Uint8Array(ev.data) : new Uint8Array(ev.data);
    let plaintext;
    try {
      plaintext = session.decrypt(data);
    } catch (e) {
      console.warn("decrypt failed:", e);
      return;
    }
    const frames = decoder.push(plaintext);
    for (const f of frames) incoming.push(f);
    if (handshakeBuf.waiter && handshakeBuf.bytes.length >= 4) {
      const w = handshakeBuf.waiter;
      handshakeBuf.waiter = null;
      w();
    }
  });

  let nodeId = null;
  if (session.remoteStaticPub) {
    nodeId = await nodeIdFromRemoteStatic(session.remoteStaticPub);
  }

  let closed = false;
  dc.addEventListener("close", () => {
    closed = true;
  });

  return {
    /** Send a single Frame to the remote peer. */
    send(frame) {
      if (closed) throw new Error("data channel closed");
      const encoded = encodeFrame(frame);
      const ct = session.encrypt(encoded);
      dc.send(ct);
    },
    /** Async iterator of incoming decrypted Frames. */
    async next() {
      while (incoming.length === 0) {
        if (closed) return { done: true };
        await new Promise((r) => setTimeout(r, 10));
      }
      return { value: incoming.shift(), done: false };
    },
    remoteNodeId() {
      return nodeId;
    },
    close() {
      try { dc.close(); } catch (e) {}
      try { pc.close(); } catch (e) {}
      if (opts && opts.shutdown) opts.shutdown();
    },
  };
}

function onDcMessage(ev, handshakeBuf, incoming) {
  const data =
    ev.data instanceof ArrayBuffer
      ? new Uint8Array(ev.data)
      : ev.data instanceof Blob
      ? null
      : new Uint8Array(ev.data);
  if (data === null) return; // ignore Blob for the demo
  const next = new Uint8Array(handshakeBuf.bytes.length + data.length);
  next.set(handshakeBuf.bytes, 0);
  next.set(data, handshakeBuf.bytes.length);
  handshakeBuf.bytes = next;
  if (handshakeBuf.waiter) {
    const w = handshakeBuf.waiter;
    handshakeBuf.waiter = null;
    w();
  }
}

function waitForOpen(dc) {
  return new Promise((resolve, reject) => {
    if (dc.readyState === "open") return resolve();
    const onOpen = () => {
      dc.removeEventListener("open", onOpen);
      resolve();
    };
    const onError = (e) => reject(new Error(`data channel error: ${e}`));
    dc.addEventListener("open", onOpen);
    dc.addEventListener("error", onError);
  });
}

function waitForDataChannel(pc, label) {
  return new Promise((resolve) => {
    pc.addEventListener(
      "datachannel",
      (ev) => {
        if (ev.channel.label === label) resolve(ev.channel);
      },
      { once: true },
    );
  });
}
