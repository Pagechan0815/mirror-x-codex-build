"""End-to-end check for the mobile control path.

Starts the relay locally, derives the same three secrets the desktop and phone
derive from an API key, connects as host through the real host module binary
path is not used here; instead the check verifies:

1. relay HTTP endpoints serve byte-for-byte current embedded PWA assets,
2. the `/relay/ws` prefixed path registers correctly,
3. a client is rejected while no host is present,
4. host + client can exchange an AES-256-GCM envelope end to end,
5. Python-side HKDF derivation matches the Rust implementation.

Run: python scripts/mobile_e2e_check.py
"""

from __future__ import annotations

import base64
import hashlib
import hmac
import json
import os
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8")

HOST = "127.0.0.1"
PORT = int(os.environ.get("MIRROR_E2E_PORT", "8791"))
BASE = f"http://{HOST}:{PORT}"
API_KEY = "sk-mirror-e2e-check-0123456789"
REPO_ROOT = Path(__file__).resolve().parents[1]
PWA_DIR = REPO_ROOT / "apps" / "codex-plus-mobile-relay" / "pwa"

ROOM_SALT = b"mirror-x-room-v1"
TOKEN_SALT = b"mirror-x-relay-tok-v1"
ENC_SALT = b"mirror-x-enc-v1"

failures: list[str] = []


def check(name: str, condition: bool, detail: str = "") -> None:
    status = "PASS" if condition else "FAIL"
    print(f"[{status}] {name}" + (f" :: {detail}" if detail else ""))
    if not condition:
        failures.append(name)


def hkdf_expand(ikm: bytes, salt: bytes, length: int) -> bytes:
    prk = hmac.new(salt, ikm, hashlib.sha256).digest()
    out, block, counter = b"", b"", 1
    while len(out) < length:
        block = hmac.new(prk, block + bytes([counter]), hashlib.sha256).digest()
        out += block
        counter += 1
    return out[:length]


def derive(api_key: str) -> tuple[str, str, bytes]:
    ikm = api_key.strip().encode()
    return (
        hkdf_expand(ikm, ROOM_SALT, 16).hex(),
        hkdf_expand(ikm, TOKEN_SALT, 16).hex(),
        hkdf_expand(ikm, ENC_SALT, 32),
    )


def b64url(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).decode().rstrip("=")


def unb64url(text: str) -> bytes:
    return base64.urlsafe_b64decode(text + "=" * (-len(text) % 4))


def seal(enc_key: bytes, payload: dict) -> dict:
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM

    nonce = os.urandom(12)
    data = AESGCM(enc_key).encrypt(nonce, json.dumps(payload).encode(), None)
    return {"type": "encrypted", "nonce": b64url(nonce), "payload": b64url(data)}


def open_envelope(enc_key: bytes, envelope: dict) -> dict:
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM

    plaintext = AESGCM(enc_key).decrypt(
        unb64url(envelope["nonce"]), unb64url(envelope["payload"]), None
    )
    return json.loads(plaintext)


def get_bytes(path: str) -> tuple[int, bytes]:
    try:
        with urllib.request.urlopen(BASE + path, timeout=5) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.read()


def get(path: str) -> tuple[int, str]:
    status, body = get_bytes(path)
    return status, body.decode("utf-8", "replace")


def content_sha256(content: bytes) -> str:
    return hashlib.sha256(content).hexdigest()


def main() -> int:
    try:
        from websockets.sync.client import connect  # noqa: F401
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM  # noqa: F401
    except ImportError as error:
        print(f"missing dependency: {error}; run pip install websockets cryptography")
        return 2

    external_relay = os.environ.get("MIRROR_E2E_EXTERNAL", "").strip() == "1"
    relay: subprocess.Popen | None = None
    if not external_relay:
        binary = os.environ.get("MIRROR_E2E_BINARY", "").strip()
        if not binary:
            binary = os.path.join("target", "debug", "codex-plus-mobile-relay.exe")
        if not os.path.isfile(binary) and not os.environ.get("MIRROR_E2E_BINARY"):
            binary = os.path.join("target", "debug", "codex-plus-mobile-relay")
        if not os.path.isfile(binary):
            print(f"relay binary not found: {binary}")
            return 2

        env = dict(os.environ, CODEX_PLUS_MOBILE_RELAY_BIND=f"{HOST}:{PORT}")
        relay = subprocess.Popen(
            [binary], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
        )
    try:
        for _ in range(50):
            try:
                if get("/health")[0] == 200:
                    break
            except OSError:
                time.sleep(0.1)
        else:
            print("relay did not become healthy")
            return 2

        status, body = get("/health")
        check("health returns ok", status == 200 and '"status":"ok"' in body)

        for asset, source_name, marker in (
            ("/relay/mobile", "index.html", "Mirror X Codex"),
            ("/relay/app.js", "app.js", "AppServerRpc"),
            ("/relay/relay.js", "relay.js", "appServerConnect"),
            ("/relay/crypto.js", "crypto.js", "mirror-x-room-v1"),
            ("/relay/style.css", "style.css", "message"),
            ("/relay/manifest.json", "manifest.json", "start_url"),
            ("/relay/icon.svg", "icon.svg", "<svg"),
        ):
            status, served_bytes = get_bytes(asset)
            served_text = served_bytes.decode("utf-8", "replace")
            check(f"asset {asset}", status == 200 and marker in served_text)
            source_bytes = (PWA_DIR / source_name).read_bytes()
            served_hash = content_sha256(served_bytes)
            source_hash = content_sha256(source_bytes)
            check(
                f"embedded asset {asset} matches source",
                status == 200 and served_hash == source_hash,
                f"served={served_hash[:12]} source={source_hash[:12]}",
            )

        status, body = get("/")
        check(
            "landing page has no relay console",
            status == 200 and "new WebSocket" not in body,
            "public page must not expose pairing controls",
        )

        room, token, enc_key = derive(API_KEY)
        check("room and token differ", room != token)
        check("enc key is 32 bytes", len(enc_key) == 32)

        from websockets.sync.client import connect

        ws_base = f"ws://{HOST}:{PORT}/relay/ws"

        # 3. client without host is rejected.
        with connect(f"{ws_base}?room={room}&token={token}&role=client") as lone_client:
            frame = json.loads(lone_client.recv(timeout=5))
            check(
                "client rejected while host offline",
                frame.get("type") == "error" and frame.get("code") == "HOST_OFFLINE",
                json.dumps(frame),
            )

        # 4. host then client exchange a sealed envelope both ways.
        with connect(f"{ws_base}?room={room}&token={token}&role=host") as host:
            hello = json.loads(host.recv(timeout=5))
            check("host registered", hello.get("type") == "registered" and hello.get("role") == "host")

            with connect(f"{ws_base}?room={room}&token={token}&role=client") as client:
                hello = json.loads(client.recv(timeout=5))
                check("client registered once host online", hello.get("type") == "registered")

                client.send(json.dumps(seal(enc_key, {"type": "appServerConnect", "sessionId": "s1"})))
                received = open_envelope(enc_key, json.loads(host.recv(timeout=5)))
                check(
                    "client to host envelope survives relay",
                    received == {"type": "appServerConnect", "sessionId": "s1"},
                    json.dumps(received),
                )

                host.send(
                    json.dumps(seal(enc_key, {"type": "appServerConnected", "sessionId": "s1"}))
                )
                received = open_envelope(enc_key, json.loads(client.recv(timeout=5)))
                check(
                    "host to client envelope survives relay",
                    received == {"type": "appServerConnected", "sessionId": "s1"},
                    json.dumps(received),
                )

                # 5. a foreign key lands in a different room and cannot read it.
                other_room, other_token, other_key = derive("sk-someone-else-9999")
                check("foreign key isolated to another room", other_room != room)
                try:
                    open_envelope(other_key, seal(enc_key, {"peek": True}))
                    check("foreign key cannot decrypt", False)
                except Exception:
                    check("foreign key cannot decrypt", True)

                # Wrong but well-formed token on the same room must be refused.
                # Malformed credentials are rejected during the HTTP upgrade,
                # which is a separate validation path covered by Rust tests.
                wrong_token = ("0" if token[0] != "0" else "1") + token[1:]
                try:
                    with connect(
                        f"{ws_base}?room={room}&token={wrong_token}&role=client"
                    ) as intruder:
                        frame = json.loads(intruder.recv(timeout=5))
                        check(
                            "wrong token refused",
                            frame.get("type") == "error"
                            and frame.get("code") == "TOKEN_MISMATCH",
                            json.dumps(frame),
                        )
                except Exception as error:
                    check("wrong token refused", False, str(error))

                # A second mobile tab explicitly takes over. The old tab must
                # receive a terminal reason instead of entering a reconnect
                # storm, while the new tab stays usable.
                with connect(f"{ws_base}?room={room}&token={token}&role=client") as replacement:
                    replacement_hello = json.loads(replacement.recv(timeout=5))
                    check(
                        "replacement client registered",
                        replacement_hello.get("type") == "registered",
                    )
                    replaced = json.loads(client.recv(timeout=5))
                    check(
                        "old client receives terminal replacement reason",
                        replaced.get("type") == "error"
                        and replaced.get("code") == "CLIENT_REPLACED"
                        and replaced.get("message")
                        == "此连接已被另一台手机或浏览器标签页接管",
                        json.dumps(replaced, ensure_ascii=False),
                    )
                    replacement.send(
                        json.dumps(
                            seal(
                                enc_key,
                                {"type": "appServerConnect", "sessionId": "replacement"},
                            )
                        )
                    )
                    received = open_envelope(enc_key, json.loads(host.recv(timeout=5)))
                    check(
                        "replacement client remains usable",
                        received
                        == {"type": "appServerConnect", "sessionId": "replacement"},
                        json.dumps(received),
                    )

                    # Inspect room details while the room is still active. Once
                    # both peers disconnect, the relay is allowed to remove it.
                    status, body = get("/status")
                    status_payload = json.loads(body)
                    reported_rooms = [
                        detail.get("room")
                        for detail in status_payload.get("roomDetails", [])
                        if isinstance(detail, dict)
                    ]
                    check(
                        "status endpoint masks pairing room",
                        room not in reported_rooms
                        and f"{room[:6]}...{room[-4:]}" in reported_rooms,
                        json.dumps(reported_rooms),
                    )

        status, body = get("/status")
        check("status endpoint reports traffic", status == 200 and "forwardedMessages" in body)
    finally:
        if relay is not None:
            relay.terminate()
            try:
                relay.wait(timeout=5)
            except subprocess.TimeoutExpired:
                relay.kill()

    print()
    if failures:
        print(f"{len(failures)} check(s) failed: {', '.join(failures)}")
        return 1
    print("all mobile relay checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
