import { webcrypto } from "node:crypto";

globalThis.crypto ??= webcrypto;
const { deriveKeyMaterial } = await import("../apps/codex-plus-mobile-relay/pwa/crypto.js");

const apiKey = "sk-mirror-compatibility-vector";
const expected = {
  room: "6aadbd99b0008e37cd094c3dfc6410c8",
  token: "5218647ff081263ed5591794de0a6175",
  enc: "fc10087f6a578139e7711377fe23583a0970ad667e0579d351d4d225fa62bb9f",
};

const hex = (bytes) => Buffer.from(bytes).toString("hex");
const native = await deriveKeyMaterial(apiKey, false);
const fallback = await deriveKeyMaterial(apiKey, true);
const actualNative = {
  room: hex(native.roomBytes),
  token: hex(native.tokenBytes),
  enc: hex(native.encBytes),
};
const actualFallback = {
  room: hex(fallback.roomBytes),
  token: hex(fallback.tokenBytes),
  enc: hex(fallback.encBytes),
};

if (JSON.stringify(actualNative) !== JSON.stringify(expected)) {
  throw new Error(`native HKDF vector mismatch: ${JSON.stringify(actualNative)}`);
}
if (JSON.stringify(actualFallback) !== JSON.stringify(expected)) {
  throw new Error(`fallback HKDF vector mismatch: ${JSON.stringify(actualFallback)}`);
}

const nativeCrypto = globalThis.crypto;
const hkdfUnsupportedCrypto = {
  getRandomValues: nativeCrypto.getRandomValues.bind(nativeCrypto),
  subtle: {
    importKey(...args) {
      if (args[2] === "HKDF") {
        return Promise.reject(new DOMException("HKDF unsupported", "NotSupportedError"));
      }
      return nativeCrypto.subtle.importKey(...args);
    },
    deriveBits: nativeCrypto.subtle.deriveBits.bind(nativeCrypto.subtle),
    sign: nativeCrypto.subtle.sign.bind(nativeCrypto.subtle),
    encrypt: nativeCrypto.subtle.encrypt.bind(nativeCrypto.subtle),
    decrypt: nativeCrypto.subtle.decrypt.bind(nativeCrypto.subtle),
  },
};
const automaticFallback = await deriveKeyMaterial(apiKey, false, hkdfUnsupportedCrypto);
const actualAutomaticFallback = {
  room: hex(automaticFallback.roomBytes),
  token: hex(automaticFallback.tokenBytes),
  enc: hex(automaticFallback.encBytes),
};
if (JSON.stringify(actualAutomaticFallback) !== JSON.stringify(expected)) {
  throw new Error(
    `automatic HKDF fallback mismatch: ${JSON.stringify(actualAutomaticFallback)}`,
  );
}
console.log("mobile crypto native/manual/automatic fallback vectors passed");
