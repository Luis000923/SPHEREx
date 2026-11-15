// Base64 codec for row-major float32 little-endian pixel payloads
// (Frame.data_b64 in shared/openapi-contract.json). Pure JS, no DOM/Buffer.
// Decoding prefers the platform's native atob (a single C++ pass) over a
// per-quad JS loop — ~5x faster on the 196 kB payloads the backend sends,
// which matters when a 24-frame series decodes on the main thread.

const B64_ALPHABET =
  "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const B64_REVOKE: Record<string, number> = {};
for (let i = 0; i < B64_ALPHABET.length; i++) {
  const ch = B64_ALPHABET.charAt(i);
  if (ch !== "") B64_REVOKE[ch] = i;
}

// Strict shape: alphabet chars only, with up to two '=' of padding at the
// very end. atob is more permissive, so validate before delegating.
const B64_RE = /^[A-Za-z0-9+/]*={0,2}$/;

// Fallback decoder for environments without atob (kept identical to the
// original per-quad implementation).
function decodeQuad(clean: string, out: Uint8Array): Uint8Array {
  let o = 0;
  for (let i = 0; i < clean.length; i += 4) {
    const chunk = clean.slice(i, i + 4);
    const pad = chunk.indexOf("=");
    const v =
      ((B64_REVOKE[chunk.charAt(0)] ?? -1) << 18) |
      ((B64_REVOKE[chunk.charAt(1)] ?? -1) << 12) |
      ((pad === 2 ? 0 : (B64_REVOKE[chunk.charAt(2)] ?? -1)) << 6) |
      (pad !== -1 ? 0 : (B64_REVOKE[chunk.charAt(3)] ?? -1));
    if (v < 0) throw new Error("invalid base64 character");
    const b0 = (v >> 16) & 0xff;
    const b1 = (v >> 8) & 0xff;
    const b2 = v & 0xff;
    if (o < out.length) out[o++] = b0;
    if (pad !== 2 && o < out.length) out[o++] = b1;
    if (pad === -1 && o < out.length) out[o++] = b2;
  }
  return out.subarray(0, o);
}

export function base64ToBytes(b64: string): Uint8Array {
  const clean = b64.replace(/[\r\n\s]/g, "");
  if (clean.length % 4 !== 0) throw new Error("invalid base64 length");
  if (!B64_RE.test(clean)) throw new Error("invalid base64 character");
  if (typeof atob === "function") {
    const bin = atob(clean);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  }
  return decodeQuad(clean, new Uint8Array((clean.length / 4) * 3));
}

export function base64ToFloat32(b64: string): Float32Array {
  const bytes = base64ToBytes(b64);
  if (bytes.length % 4 !== 0)
    throw new Error("float32 payload is not a multiple of 4 bytes");
  const copy = new Uint8Array(bytes.length);
  copy.set(bytes);
  return new Float32Array(copy.buffer);
}

export function float32ToBase64(data: Float32Array): string {
  const bytes = new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
  let out = "";
  for (let i = 0; i < bytes.length; i += 3) {
    const b0 = bytes[i] ?? 0;
    const b1 = bytes[i + 1];
    const b2 = bytes[i + 2];
    const v = (b0 << 16) | ((b1 ?? 0) << 8) | (b2 ?? 0);
    out += B64_ALPHABET.charAt((v >> 18) & 63);
    out += B64_ALPHABET.charAt((v >> 12) & 63);
    out += b1 === undefined ? "=" : B64_ALPHABET.charAt((v >> 6) & 63);
    out += b2 === undefined ? "=" : B64_ALPHABET.charAt(v & 63);
  }
  return out;
}
