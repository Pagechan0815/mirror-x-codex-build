// Key derivation and AES-GCM helpers.
//
// The phone can either derive credentials from the user's API key or restore a
// narrowed pairing bundle that contains only relay-scoped secrets.

const ROOM_SALT = "mirror-x-room-v1";
const TOKEN_SALT = "mirror-x-relay-tok-v1";
const ENC_SALT = "mirror-x-enc-v1";

const encoder = new TextEncoder();
const decoder = new TextDecoder();

function toHex(bytes) {
  return Array.from(bytes)
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

export function b64urlEncode(bytes) {
  let binary = "";
  const view = new Uint8Array(bytes);
  for (let index = 0; index < view.length; index += 1) {
    binary += String.fromCharCode(view[index]);
  }
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/g, "");
}

export function b64urlDecode(text) {
  const normalized = String(text || "").replaceAll("-", "+").replaceAll("_", "/");
  const padding = (4 - (normalized.length % 4 || 4)) % 4;
  const binary = atob(normalized + "=".repeat(padding));
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return bytes;
}

function requireWebCrypto(webCrypto = globalThis.crypto) {
  if (!webCrypto?.subtle || !webCrypto?.getRandomValues) {
    throw new Error("当前浏览器缺少安全加密能力，请使用系统自带浏览器扫码打开。");
  }
  return webCrypto;
}

async function hmacSha256(keyBytes, dataBytes, cryptoProvider) {
  const webCrypto = requireWebCrypto(cryptoProvider);
  const key = await webCrypto.subtle.importKey(
    "raw",
    keyBytes,
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  return new Uint8Array(await webCrypto.subtle.sign("HMAC", key, dataBytes));
}

async function hkdfHmacFallback(keyMaterial, salt, length, cryptoProvider) {
  // RFC 5869 HKDF-Extract + HKDF-Expand, with empty info. HMAC-SHA256 is
  // supported by more mobile browsers than the WebCrypto HKDF algorithm.
  const prk = await hmacSha256(encoder.encode(salt), keyMaterial, cryptoProvider);
  const output = new Uint8Array(length);
  let previous = new Uint8Array(0);
  let offset = 0;
  let counter = 1;

  while (offset < length) {
    const input = new Uint8Array(previous.length + 1);
    input.set(previous, 0);
    input[input.length - 1] = counter;
    previous = await hmacSha256(prk, input, cryptoProvider);
    const remaining = length - offset;
    output.set(previous.subarray(0, Math.min(previous.length, remaining)), offset);
    offset += Math.min(previous.length, remaining);
    counter += 1;
  }
  return output;
}

async function hkdf(
  keyMaterial,
  salt,
  length,
  forceFallback = false,
  cryptoProvider = globalThis.crypto,
) {
  const webCrypto = requireWebCrypto(cryptoProvider);
  if (!forceFallback) {
    try {
      const baseKey = await webCrypto.subtle.importKey(
        "raw",
        keyMaterial,
        "HKDF",
        false,
        ["deriveBits"],
      );
      const bits = await webCrypto.subtle.deriveBits(
        {
          name: "HKDF",
          hash: "SHA-256",
          salt: encoder.encode(salt),
          info: new Uint8Array(0),
        },
        baseKey,
        length * 8,
      );
      return new Uint8Array(bits);
    } catch {
      // Older Safari and some Android vendor browsers expose WebCrypto but do
      // not implement HKDF. Fall back to equivalent HMAC-SHA256 operations.
    }
  }
  return hkdfHmacFallback(keyMaterial, salt, length, webCrypto);
}

async function importEncKey(rawBytes) {
  return requireWebCrypto().subtle.importKey(
    "raw",
    rawBytes,
    "AES-GCM",
    false,
    ["encrypt", "decrypt"],
  );
}

export async function deriveKeyMaterial(
  apiKey,
  forceFallback = false,
  cryptoProvider = globalThis.crypto,
) {
  const ikm = encoder.encode(String(apiKey || "").trim());
  const [roomBytes, tokenBytes, encBytes] = await Promise.all([
    hkdf(ikm, ROOM_SALT, 16, forceFallback, cryptoProvider),
    hkdf(ikm, TOKEN_SALT, 16, forceFallback, cryptoProvider),
    hkdf(ikm, ENC_SALT, 32, forceFallback, cryptoProvider),
  ]);
  return { roomBytes, tokenBytes, encBytes };
}

/// Derives { roomId, relayToken, encKey } from the raw API key.
export async function deriveKeys(apiKey) {
  const { roomBytes, tokenBytes, encBytes } = await deriveKeyMaterial(apiKey);
  return {
    roomId: toHex(roomBytes),
    relayToken: toHex(tokenBytes),
    encBytes,
    encKey: await importEncKey(encBytes),
  };
}

export async function restoreKeys(pairing) {
  if (!pairing || Number(pairing.version) !== 1) {
    throw new Error("Unsupported pairing bundle");
  }
  const encBytes = b64urlDecode(pairing.encKey);
  if (encBytes.length !== 32) {
    throw new Error("Pairing bundle encryption key is invalid");
  }
  return {
    roomId: String(pairing.roomId || ""),
    relayToken: String(pairing.relayToken || ""),
    encKey: await importEncKey(encBytes),
  };
}

export function decodePairingFragment(fragment) {
  const text = String(fragment || "").replace(/^#/, "").trim();
  if (!text) return null;
  const payload = text.startsWith("mx=") ? text.slice(3) : text;
  const bytes = b64urlDecode(payload);
  return JSON.parse(decoder.decode(bytes));
}

export async function encryptJson(encKey, payload) {
  const webCrypto = requireWebCrypto();
  const nonce = webCrypto.getRandomValues(new Uint8Array(12));
  const plaintext = encoder.encode(JSON.stringify(payload));
  const ciphertext = await webCrypto.subtle.encrypt(
    { name: "AES-GCM", iv: nonce },
    encKey,
    plaintext,
  );
  return {
    type: "encrypted",
    nonce: b64urlEncode(nonce),
    payload: b64urlEncode(ciphertext),
  };
}

export async function decryptJson(encKey, envelope) {
  if (!envelope || envelope.type !== "encrypted") {
    throw new Error("Received a non-encrypted relay envelope");
  }
  const nonce = b64urlDecode(envelope.nonce);
  const ciphertext = b64urlDecode(envelope.payload);
  const plaintext = await requireWebCrypto().subtle.decrypt(
    { name: "AES-GCM", iv: nonce },
    encKey,
    ciphertext,
  );
  return JSON.parse(decoder.decode(plaintext));
}
