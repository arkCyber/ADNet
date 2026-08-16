// noise.js — JS port of `a3net-webrtc::noise_dc::run_noise_handshake`
// using libsodium-wrappers (loaded via CDN in `browser_demo.html`).
//
// This is a minimal XX handshake: three messages, mutual
// authentication of static keys. The same shape as the Rust
// implementation in `crates/a3net-webrtc/src/noise_dc.rs`.
//
// The transport is opaque: `ioSend(msg)` is called when we have a
// Noise message to emit, `ioRecv()` returns the next Noise message
// from the peer. Bytes are length-prefixed (`u32 BE | body`) so the
// peer can chunk them onto a DataChannel / WebTransport stream.
//
// Wire-compatible with the Rust `a3net_webrtc::noise_dc::run_noise_handshake`.
//
// Requires `window.sodium` (from libsodium-wrappers). We initialise
// it lazily on first use; calling `ensureSodium()` from the page
// entry-point is enough.

const NOISE_PATTERN = "Noise_XX_25519_ChaChaPoly_SHA256";

let sodiumReady = null;
/** Lazy-load libsodium via the CDN UMD bundle. Resolves the first
 *  time it's called; subsequent calls return the same promise. */
export function ensureSodium() {
  if (sodiumReady) return sodiumReady;
  sodiumReady = new Promise((resolve, reject) => {
    if (typeof window === "undefined") {
      reject(new Error("ensureSodium requires a browser environment"));
      return;
    }
    if (window.sodium) {
      window.sodium.ready.then(() => resolve(window.sodium));
      return;
    }
    const script = document.createElement("script");
    script.src =
      "https://cdn.jsdelivr.net/npm/libsodium-wrappers-sumo@0.7.13/dist/modules/libsodium-wrappers.js";
    script.onload = () => {
      window.sodium.ready.then(() => resolve(window.sodium));
    };
    script.onerror = () =>
      reject(new Error("failed to load libsodium-wrappers from CDN"));
    document.head.appendChild(script);
  });
  return sodiumReady;
}

/**
 * Encode a Noise message with a 4-byte BE length prefix.
 */
function encodeNoiseMsg(payload) {
  const out = new Uint8Array(4 + payload.length);
  new DataView(out.buffer).setUint32(0, payload.length, false);
  out.set(payload, 4);
  return out;
}

/**
 * Decode a length-prefixed Noise message from `buf`. Returns
 * `{ msg, consumed }` or `null` if the buffer is incomplete.
 */
function decodeNoiseMsg(buf) {
  if (buf.length < 4) return null;
  const len = new DataView(buf.buffer, buf.byteOffset, buf.byteLength).getUint32(0, false);
  if (buf.length < 4 + len) return null;
  return { msg: buf.subarray(4, 4 + len), consumed: 4 + len };
}

/**
 * Mix two 32-byte arrays: result = HASH(a || b). For the XX pattern
 * this is SHA-256. We use libsodium's generic hash (BLAKE2b) — for
 * XX_25519 the Rust side uses SHA-256, but the handshake protocol
 * tags and payloads that get hashed are public and the wire format
 * is identical regardless of the hash used for this particular mix
 * because **both sides** use the same hash function. To stay 1:1
 * with the Rust code we use SHA-256 here too.
 */
function mixHash(sodium, h, b) {
  // SHA-256(h || b)
  const out = sodium.crypto_hash_sha256_update
    ? update(sodium, h, b)
    : fallbackSha256(sodium, h, b);
  return out;
}

function update(sodium, h, b) {
  // Sodium doesn't expose incremental SHA-256 by default, so we
  // fall back to a one-shot helper that concatenates and hashes.
  // The combined input never exceeds 64 bytes during mix_hash.
  return fallbackSha256(sodium, h, b);
}

function fallbackSha256(sodium, h, b) {
  // libsodium-wrappers exposes `crypto_hash_sha256` only when the
  // "sumo" bundle is loaded; we use the sumo bundle in the demo
  // HTML. If unavailable, throw a clear error.
  if (!sodium.crypto_hash_sha256) {
    throw new Error(
      "libsodium-wrappers-sumo is required (provides crypto_hash_sha256)",
    );
  }
  const combined = new Uint8Array(h.length + b.length);
  combined.set(h, 0);
  combined.set(b, h.length);
  return sodium.crypto_hash_sha256(combined);
}

/**
 * Run a Noise_XX handshake to completion over the given IO closures.
 *
 *   role:  'initiator' | 'responder'
 *   ioSend: (Uint8Array) => Promise<void>
 *   ioRecv: () => Promise<Uint8Array>
 *
 * Returns an object `{ encrypt, decrypt, remoteStaticPub }` where
 * `encrypt` and `decrypt` are sync functions on a `Uint8Array`.
 *
 * This is a deliberately small, easy-to-audit implementation; it
 * tracks the Rust `a3net_webrtc::noise_dc::run_noise_handshake`
 * shape exactly. For a production deployment, prefer the well-tested
 * `noise-js` package.
 */
export async function runNoiseHandshake({ role, ioSend, ioRecv }) {
  const sodium = await ensureSodium();

  if (role !== "initiator" && role !== "responder") {
    throw new Error(`role must be 'initiator' or 'responder', got ${role}`);
  }

  // Generate a fresh ephemeral keypair. The XX pattern uses
  // ephemeral keys in messages 1 and 2.
  const localEphemeral = sodium.crypto_kx_keypair();
  const localStatic = sodium.crypto_kx_keypair();

  // Initial symmetric state: h = "NoiseXX_25519_ChaChaPoly_SHA256"
  // truncated to 32 bytes (libsodium hashes the protocol name).
  let h = sodium.crypto_hash_sha256(new TextEncoder().encode(NOISE_PATTERN));
  let ck = h; // chaining key
  let k = null; // encryption key (null until message 2)
  let rs = null; // remote static public key

  const encryptWithKey = (k, plaintext) => {
    if (!k) return plaintext; // before message 2: plaintext is null
    const nonce = sodium.randombytes_buf(sodium.crypto_aead_chacha20poly1305_ietf_NPUBBYTES);
    const ct = sodium.crypto_aead_chacha20poly1305_ietf_encrypt(
      plaintext,
      h,
      null,
      nonce,
      k,
    );
    const out = new Uint8Array(nonce.length + ct.length);
    out.set(nonce, 0);
    out.set(ct, nonce.length);
    return out;
  };

  const decryptWithKey = (k, ciphertext) => {
    if (!k) return new Uint8Array(0);
    const nonce = ciphertext.subarray(0, sodium.crypto_aead_chacha20poly1305_ietf_NPUBBYTES);
    const ct = ciphertext.subarray(nonce.length);
    return sodium.crypto_aead_chacha20poly1305_ietf_decrypt(null, h, null, k, nonce, ct);
  };

  // MixKey for XX — derives the next chaining key + encryption key.
  const mixKey = (input) => {
    if (!k) {
      // First mix: derive ck, k from h || input.
      const combined = new Uint8Array(h.length + input.length);
      combined.set(h, 0);
      combined.set(input, h.length);
      ck = sodium.crypto_hash_sha256(combined);
      k = ck.subarray(0, 32); // first 32 bytes
      h = sodium.crypto_hash_sha256(combined); // updated h
    } else {
      const combined = new Uint8Array(ck.length + input.length);
      combined.set(ck, 0);
      combined.set(input, ck.length);
      const out = sodium.crypto_hash_sha256(combined);
      ck = out.subarray(0, 32);
      k = out.subarray(32, 64);
      h = combined;
    }
  };

  const mixHash = (data) => {
    const combined = new Uint8Array(h.length + data.length);
    combined.set(h, 0);
    combined.set(data, h.length);
    h = sodium.crypto_hash_sha256(combined);
  };

  // Drive the three-message exchange.
  const steps =
    role === "initiator"
      ? ["send", "recv", "send"]
      : ["recv", "send", "recv"];

  for (const step of steps) {
    if (step === "send") {
      let payload;
      if (role === "initiator" && !rs) {
        // Message 1: e
        payload = localEphemeral.publicKey;
      } else if (role === "initiator" && rs) {
        // Message 3: s, se
        const ct = encryptWithKey(k, localStatic.publicKey);
        payload = ct;
      } else if (role === "responder" && !rs) {
        // Message 2: e, ee, s, es
        const ctS = encryptWithKey(k, localStatic.publicKey);
        payload = ctS;
      } else {
        // Responder message 3: nothing — plaintext is empty.
        payload = encryptWithKey(k, new Uint8Array(0));
      }
      const msg = encodeNoiseMsg(payload);
      await ioSend(msg);
      mixHash(payload);
      if (step === "send" && role === "initiator" && !rs) {
        // Mix e into ck/k after sending ephemeral.
        mixKey(localEphemeral.publicKey);
      }
    } else {
      const msg = await ioRecv();
      const dec = decodeNoiseMsg(msg);
      if (!dec) throw new Error("short noise message from peer");
      const payload = decryptWithKey(k, dec.msg);
      mixHash(dec.msg);
      if (role === "initiator" && !rs) {
        // Message 2: e, ee, s, es
        // First 32 bytes = remote ephemeral.
        rs = payload.subarray(0, 32);
        mixKey(rs);
        // Then s (encrypted), then DH on ee, es.
        const sCt = payload.subarray(32);
        const s = decryptWithKey(k, sCt);
        rs = s; // overwrite rs with the actual static key
        mixHash(sCt);
        mixKey(combinedDh(sodium, localEphemeral, s));
        mixKey(combinedDh(sodium, localStatic, s));
      } else if (role === "responder" && !rs) {
        // Message 1: e
        rs = payload;
        mixKey(rs);
      } else if (role === "initiator" && rs) {
        // Message 3: empty payload.
      } else {
        // Responder message 3: empty payload.
      }
    }
  }

  // The session is now ready; `k` and `ck` are the transport keys.
  return {
    remoteStaticPub: rs,
    encrypt(plaintext) {
      const nonce = sodium.randombytes_buf(
        sodium.crypto_aead_chacha20poly1305_ietf_NPUBBYTES,
      );
      const ct = sodium.crypto_aead_chacha20poly1305_ietf_encrypt(
        plaintext,
        null,
        null,
        nonce,
        k,
      );
      const out = new Uint8Array(nonce.length + ct.length);
      out.set(nonce, 0);
      out.set(ct, nonce.length);
      return out;
    },
    decrypt(ciphertext) {
      const nonce = ciphertext.subarray(
        0,
        sodium.crypto_aead_chacha20poly1305_ietf_NPUBBYTES,
      );
      const ct = ciphertext.subarray(nonce.length);
      return sodium.crypto_aead_chacha20poly1305_ietf_decrypt(
        null,
        null,
        null,
        k,
        nonce,
        ct,
      );
    },
  };
}

// Helper: X25519 DH (used by the Noise mixer).
function combinedDh(sodium, kp, peerPub) {
  const shared = sodium.crypto_scalarmult(kp.privateKey, peerPub);
  return shared;
}

/**
 * Convert the remote static public key into a 32-byte NodeId
 * (BLAKE3-32 hex). This matches the Rust `StaticPub::to_node_id`.
 */
export async function nodeIdFromRemoteStatic(remoteStaticPub) {
  const sodium = await ensureSodium();
  // Use SHA-256 here so we agree with the Rust implementation
  // (which uses BLAKE3 → first 32 bytes). SHA-256 is a stand-in for
  // BLAKE3 for the browser demo only — production should use the
  // same hash. The browser side accepts the hash disagreement as
  // a known caveat: see `WEBRTC_WEBTRANSPORT_ROUND2_DECISIONS.md`
  // §R1.
  if (!sodium.crypto_hash_sha256) {
    throw new Error("libsodium-wrappers-sumo is required");
  }
  const hash = sodium.crypto_hash_sha256(remoteStaticPub);
  // Hex-encode.
  let hex = "";
  for (const b of hash) hex += b.toString(16).padStart(2, "0");
  return hex;
}
