from __future__ import annotations

import argparse
import base64
import hashlib
import hmac
import json
import os
import sys
import threading
import time
from datetime import datetime, timezone
from typing import Any

from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from websockets.sync.client import connect

ROOM_SALT = b"mirror-x-room-v1"
TOKEN_SALT = b"mirror-x-relay-tok-v1"
ENC_SALT = b"mirror-x-enc-v1"


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


def seal(enc_key: bytes, payload: dict[str, Any]) -> dict[str, Any]:
    nonce = os.urandom(12)
    data = AESGCM(enc_key).encrypt(nonce, json.dumps(payload, ensure_ascii=False).encode(), None)
    return {"type": "encrypted", "nonce": b64url(nonce), "payload": b64url(data)}


def open_envelope(enc_key: bytes, envelope: dict[str, Any]) -> dict[str, Any]:
    plaintext = AESGCM(enc_key).decrypt(
        unb64url(envelope["nonce"]), unb64url(envelope["payload"]), None
    )
    return json.loads(plaintext)


def iso_now() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")


class DemoState:
    def __init__(
        self,
        project_cwd: str,
        active_thread: bool = False,
        control_envelope_demo: bool = False,
    ) -> None:
        self.project_cwd = project_cwd
        self.lock = threading.Lock()
        preview_image_path = os.path.join(project_cwd, "preview.png")
        large_preview_image_path = os.path.join(project_cwd, "large-preview.png")
        preview_markdown_path = os.path.join(project_cwd, "PREVIEW.md")
        preview_png = base64.b64decode(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJ"
            "AAAADUlEQVR42mNk+M/wHwAF/gL+3c2sWQAAAABJRU5ErkJggg=="
        )
        initial_items = [
            {"id": "demo-user-1", "type": "userMessage", "text": "帮我检查手机远程控制状态"},
            {
                "id": "demo-commentary-1",
                "type": "agentMessage",
                "phase": "commentary",
                "text": "我先核对连接、历史读取和文件预览链路。",
            },
            {
                "id": "demo-reasoning-1",
                "type": "reasoning",
                "summary": ["连接与历史索引正常，继续验证附件和 Markdown。"],
                "content": [],
            },
            {
                "id": "demo-command-1",
                "type": "commandExecution",
                "command": "npm run mobile-check",
                "aggregatedOutput": "检查布局\n检查附件\n全部通过",
            },
        ]
        if not active_thread:
            initial_items.append(
                {
                    "id": "demo-final-1",
                    "type": "agentMessage",
                    "phase": "final_answer",
                    "text": (
                        "已连接本地演示 Host，项目列表与会话加载正常。\n\n"
                        f"![手机端图片预览](<{preview_image_path}>)\n\n"
                        f"[查看 Markdown 说明](<{preview_markdown_path}>)"
                    ),
                }
            )
        initial_turn = {
            "id": "demo-active-turn",
            "status": {"type": "inProgress"} if active_thread else {"type": "completed"},
            "items": initial_items,
        }
        turns = [initial_turn]
        if control_envelope_demo:
            turns.insert(
                0,
                {
                    "id": "demo-control-envelope-turn",
                    "status": {"type": "completed"},
                    "items": [
                        {
                            "type": "userMessage",
                            "text": (
                                "<heartbeat>\n"
                                "<automation_id>30</automation_id>\n"
                                "<current_time_iso>2026-08-15T06:55:21.558Z</current_time_iso>\n"
                                "<instructions>每30分钟执行一次 FIRST-AGENT 经营总控。</instructions>\n"
                                "</heartbeat>"
                            ),
                        },
                        {
                            "type": "agentMessage",
                            "text": (
                                "<heartbeat>\n"
                                "<automation_id>30</automation_id>\n"
                                "<decision>DONT_NOTIFY</decision>\n"
                                "<message>任务已完成，终审结果待确认。</message>\n"
                                "</heartbeat>"
                            ),
                        },
                    ],
                },
            )
        self.threads: dict[str, dict[str, Any]] = {
            "demo-thread-1": {
                "id": "demo-thread-1",
                "cwd": project_cwd,
                "preview": "手机端远程 Codex 演示线程",
                "updatedAt": iso_now(),
                "recencyAt": iso_now(),
                "status": {"type": "active"} if active_thread else {"type": "notLoaded"},
                "turns": turns,
                "materialized": True,
                "occupied": active_thread,
            }
        }
        self.next_thread_id = 2
        self.files: dict[str, list[dict[str, Any]]] = {
            project_cwd: [
                {"fileName": "apps", "isDirectory": True, "isFile": False},
                {"fileName": "README.md", "isDirectory": False, "isFile": True},
                {"fileName": "AGENTS.md", "isDirectory": False, "isFile": True},
                {"fileName": "PREVIEW.md", "isDirectory": False, "isFile": True},
                {"fileName": "preview.png", "isDirectory": False, "isFile": True},
                {"fileName": "large-preview.png", "isDirectory": False, "isFile": True},
            ],
            os.path.join(project_cwd, "apps"): [
                {"fileName": "mobile", "isDirectory": True, "isFile": False},
            ],
            os.path.join(project_cwd, "apps", "mobile"): [
                {"fileName": "app.js", "isDirectory": False, "isFile": True},
            ],
        }
        self.file_contents = {
            os.path.join(project_cwd, "README.md"): "# Mirror X Codex\n\n手机远程工作台演示文件。\n",
            os.path.join(project_cwd, "AGENTS.md"): "# Codex\n\n默认使用中文，结论先行。\n",
            preview_markdown_path: (
                "# 手机端 Markdown 预览\n\n"
                "这份文件用于验证 **Markdown 排版**、列表和代码块。\n\n"
                "- 图片可直接查看\n"
                "- Markdown 默认渲染\n"
                "- 可切换查看源码\n\n"
                "```javascript\nconsole.log('Mirror X Mobile Preview');\n```\n"
            ),
            preview_image_path: preview_png,
            # PNG decoders ignore trailing bytes. The padding makes this a
            # deterministic >2 MiB fixture for the chunked preview protocol.
            large_preview_image_path: preview_png + bytes(2_300_000),
            os.path.join(project_cwd, "apps", "mobile", "app.js"): "console.log('mirror x mobile');\n",
        }

    def list_threads(self) -> list[dict[str, Any]]:
        with self.lock:
            result = []
            for thread in self.threads.values():
                result.append(
                    {
                        "id": thread["id"],
                        "cwd": thread["cwd"],
                        "preview": thread["preview"],
                        "updatedAt": thread["updatedAt"],
                        "recencyAt": thread["recencyAt"],
                        "status": thread.get("status", {"type": "notLoaded"}),
                    }
                )
            return result

    def start_thread(self, cwd: str | None) -> dict[str, Any]:
        with self.lock:
            thread_id = f"demo-thread-{self.next_thread_id}"
            self.next_thread_id += 1
            thread = {
                "id": thread_id,
                "cwd": cwd or self.project_cwd,
                "preview": "(空会话)",
                "updatedAt": iso_now(),
                "recencyAt": iso_now(),
                "turns": [],
                "materialized": False,
                "occupied": False,
            }
            self.threads[thread_id] = thread
            return {"id": thread_id, "cwd": thread["cwd"]}

    def turns_for(self, thread_id: str) -> list[dict[str, Any]]:
        with self.lock:
            thread = self.threads[thread_id]
            return json.loads(json.dumps(thread["turns"]))

    def ensure_materialized(
        self,
        thread_id: str,
        turn_id: str,
        user_text: str,
        agent_text: str,
        include_collab: bool = False,
    ) -> None:
        with self.lock:
            thread = self.threads[thread_id]
            thread["materialized"] = True
            thread["preview"] = user_text[:90]
            thread["updatedAt"] = iso_now()
            thread["recencyAt"] = iso_now()
            items = [
                {"id": f"user-{turn_id}", "type": "userMessage", "text": user_text},
                {
                    "id": f"commentary-{turn_id}",
                    "type": "agentMessage",
                    "phase": "commentary",
                    "text": "正在核对同步链路与移动端渲染。",
                },
                {
                    "id": f"reasoning-{turn_id}",
                    "type": "reasoning",
                    "summary": ["历史快照已加载，增量事件继续补齐。"],
                    "content": [],
                },
                {
                    "id": f"command-{turn_id}",
                    "type": "commandExecution",
                    "command": "cargo test -p codex-plus-mobile-relay",
                    "aggregatedOutput": "11 tests passed",
                    "status": "completed",
                },
            ]
            if include_collab:
                items.append(
                    {
                        "id": f"collab-{turn_id}",
                        "type": "collabAgentToolCall",
                        "taskName": "核对手机端长任务状态",
                        "status": "completed",
                    }
                )
            items.append(
                {
                    "id": f"agent-{turn_id}",
                    "type": "agentMessage",
                    "phase": "final_answer",
                    "text": agent_text,
                }
            )
            thread["turns"].append(
                {"id": turn_id, "items": items, "status": {"type": "inProgress"}}
            )


def send_rpc(ws, enc_key: bytes, session_id: str, payload: dict[str, Any]) -> None:
    ws.send(json.dumps(seal(enc_key, {"type": "appServerMessage", "sessionId": session_id, "message": json.dumps(payload, ensure_ascii=False)})))


def send_notification(ws, enc_key: bytes, session_id: str, method: str, params: dict[str, Any]) -> None:
    send_rpc(ws, enc_key, session_id, {"jsonrpc": "2.0", "method": method, "params": params})


def summarize_user_input(items: list[dict[str, Any]]) -> tuple[str, list[str]]:
    user_text = ""
    attachment_names: list[str] = []
    for item in items:
        if item.get("type") == "text":
            user_text = item.get("text", "")
        elif item.get("type") in ("localImage", "mention"):
            attachment_names.append(
                item.get("name") or os.path.basename(item.get("path", "附件"))
            )
    if attachment_names:
        user_text = f"{user_text}\n\n附件：{'、'.join(attachment_names)}".strip()
    return user_text, attachment_names


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--api-key", required=True)
    parser.add_argument(
        "--secure",
        action="store_true",
        help="Use HTTPS/WSS when connecting to a public TLS relay.",
    )
    parser.add_argument(
        "--relay-prefix",
        default="/relay",
        help="HTTP/WebSocket path prefix, for example /relay-canary-v1245.",
    )
    parser.add_argument("--project-cwd", default=r"D:\mirror++\CodexPlusPlus")
    parser.add_argument("--state-file", default="")
    parser.add_argument("--event-log", default="")
    parser.add_argument(
        "--fail-fast-thread-list-once",
        action="store_true",
        help="Fail the first state-db-only thread/list request to test fallback.",
    )
    parser.add_argument(
        "--fail-all-thread-lists",
        action="store_true",
        help="Fail every thread/list request to test initialization recovery.",
    )
    parser.add_argument("--resume-delay-ms", type=int, default=0)
    parser.add_argument(
        "--turn-response-delay-ms",
        type=int,
        default=0,
        help="Pause before agent text so scroll and running-state behavior can be inspected.",
    )
    parser.add_argument(
        "--turn-chunk-delay-ms",
        type=int,
        default=80,
        help="Pause between streamed agent text chunks.",
    )
    parser.add_argument(
        "--mode",
        choices=("standalone", "desktopSync"),
        default="standalone",
    )
    parser.add_argument(
        "--active-thread",
        action="store_true",
        help="Expose an already-running desktop turn for mobile sync tests.",
    )
    parser.add_argument(
        "--control-envelope-demo",
        action="store_true",
        help="Include heartbeat control envelopes to verify user-facing message cleanup.",
    )
    parser.add_argument(
        "--collab-agent-demo",
        action="store_true",
        help="Emit a child-agent item during each demo turn.",
    )
    args = parser.parse_args()

    room, token, enc_key = derive(args.api_key)
    bundle = {
        "version": 1,
        "roomId": room,
        "relayToken": token,
        "encKey": b64url(enc_key),
    }
    http_scheme = "https" if args.secure else "http"
    ws_scheme = "wss" if args.secure else "ws"
    default_port = (args.secure and args.port == 443) or (
        not args.secure and args.port == 80
    )
    authority = args.host if default_port else f"{args.host}:{args.port}"
    relay_prefix = f"/{args.relay_prefix.strip('/')}"
    mobile_url = (
        f"{http_scheme}://{authority}{relay_prefix}/mobile#mx="
        f"{b64url(json.dumps(bundle, ensure_ascii=False, separators=(',', ':')).encode())}"
    )
    if args.state_file:
        with open(args.state_file, "w", encoding="utf-8") as fh:
            json.dump({"mobileUrl": mobile_url, "roomId": room}, fh, ensure_ascii=False, indent=2)
    print(mobile_url, flush=True)

    state = DemoState(
        args.project_cwd,
        active_thread=args.active_thread,
        control_envelope_demo=args.control_envelope_demo,
    )
    fast_thread_list_failures = 1 if args.fail_fast_thread_list_once else 0

    def log_event(payload: dict[str, Any]) -> None:
        if not args.event_log:
            return
        with open(args.event_log, "a", encoding="utf-8") as event_file:
            event_file.write(json.dumps(payload, ensure_ascii=False) + "\n")

    ws_url = (
        f"{ws_scheme}://{authority}{relay_prefix}/ws"
        f"?room={room}&token={token}&role=host"
    )

    with connect(ws_url) as ws:
        hello = json.loads(ws.recv(timeout=10))
        if hello.get("type") != "registered":
            print(f"unexpected registration frame: {hello}", file=sys.stderr)
            return 1
        sessions: set[str] = set()
        uploads: dict[str, dict[str, Any]] = {}

        while True:
            try:
                raw = ws.recv(timeout=60)
            except TimeoutError:
                # Keep the demo host alive while a browser is opened or reloaded.
                continue
            inbound = open_envelope(enc_key, json.loads(raw))
            message_type = inbound.get("type")
            session_id = inbound.get("sessionId", "demo")

            if message_type == "appServerConnect":
                resumed = session_id in sessions
                sessions.add(session_id)
                ws.send(
                    json.dumps(
                        seal(
                            enc_key,
                            {
                                "type": "appServerConnected",
                                "sessionId": session_id,
                                "resumed": resumed,
                                "mode": args.mode,
                                "capabilities": ["fileDownloadChunks"],
                            },
                        )
                    )
                )
                continue

            if message_type and message_type.startswith("fileUpload"):
                request_id = inbound.get("requestId", "")
                upload_id = inbound.get("uploadId", "")
                response: dict[str, Any] = {
                    "type": "fileUploadResponse",
                    "sessionId": session_id,
                    "requestId": request_id,
                    "uploadId": upload_id,
                    "ok": True,
                }
                try:
                    if message_type == "fileUploadStart":
                        uploads[upload_id] = {
                            "name": inbound.get("fileName", "attachment.bin"),
                            "mimeType": inbound.get("mimeType", "application/octet-stream"),
                            "size": int(inbound.get("size", 0)),
                            "nextIndex": 0,
                            "data": bytearray(),
                        }
                        response["receivedBytes"] = 0
                    elif message_type == "fileUploadChunk":
                        upload = uploads[upload_id]
                        if int(inbound.get("index", -1)) != upload["nextIndex"]:
                            raise ValueError("chunk order mismatch")
                        upload["data"].extend(unb64url(inbound.get("data", "")))
                        upload["nextIndex"] += 1
                        response["receivedBytes"] = len(upload["data"])
                    elif message_type == "fileUploadFinish":
                        upload = uploads.pop(upload_id)
                        if len(upload["data"]) != upload["size"]:
                            raise ValueError("upload size mismatch")
                        response["path"] = os.path.join(
                            args.project_cwd,
                            ".mirror-x-mobile-uploads",
                            f"{upload_id}-{os.path.basename(upload['name'])}",
                        )
                        log_event(
                            {
                                "method": "fileUpload/finished",
                                "name": upload["name"],
                                "mimeType": upload["mimeType"],
                                "size": upload["size"],
                                "path": response["path"],
                                "time": time.time(),
                            }
                        )
                    elif message_type == "fileUploadCancel":
                        uploads.pop(upload_id, None)
                except Exception as error:
                    response["ok"] = False
                    response["error"] = str(error)
                ws.send(json.dumps(seal(enc_key, response)))
                continue

            if message_type == "fileDownloadRequest":
                request_id = inbound.get("requestId", "")
                path = inbound.get("path", "")
                max_bytes = int(inbound.get("maxBytes", 0))
                content = state.file_contents.get(path, f"Mock file: {path}\n")
                data = content if isinstance(content, bytes) else content.encode()
                response_base = {
                    "type": "fileDownloadResponse",
                    "sessionId": session_id,
                    "requestId": request_id,
                }
                if max_bytes <= 0 or len(data) > max_bytes:
                    ws.send(
                        json.dumps(
                            seal(
                                enc_key,
                                {
                                    **response_base,
                                    "ok": False,
                                    "phase": "error",
                                    "error": "文件超过手机端预览上限",
                                },
                            )
                        )
                    )
                    continue
                ws.send(
                    json.dumps(
                        seal(
                            enc_key,
                            {
                                **response_base,
                                "ok": True,
                                "phase": "start",
                                "size": len(data),
                                "receivedBytes": 0,
                            },
                        )
                    )
                )
                received = 0
                chunk_size = 256 * 1024
                for index, offset in enumerate(range(0, len(data), chunk_size)):
                    chunk = data[offset : offset + chunk_size]
                    received += len(chunk)
                    ws.send(
                        json.dumps(
                            seal(
                                enc_key,
                                {
                                    **response_base,
                                    "ok": True,
                                    "phase": "chunk",
                                    "index": index,
                                    "data": b64url(chunk),
                                    "receivedBytes": received,
                                },
                            )
                        )
                    )
                ws.send(
                    json.dumps(
                        seal(
                            enc_key,
                            {
                                **response_base,
                                "ok": True,
                                "phase": "finish",
                                "size": len(data),
                                "index": (len(data) + chunk_size - 1) // chunk_size,
                                "receivedBytes": len(data),
                            },
                        )
                    )
                )
                log_event(
                    {
                        "type": "fileDownload",
                        "path": path,
                        "bytes": len(data),
                        "chunks": (len(data) + chunk_size - 1) // chunk_size,
                    }
                )
                continue

            if message_type != "appServerMessage":
                continue

            rpc = json.loads(inbound["message"])
            method = rpc.get("method")
            rpc_id = rpc.get("id")
            params = rpc.get("params") or {}
            log_event({"method": method, "params": params, "time": time.time()})

            if method == "initialize":
                send_rpc(
                    ws,
                    enc_key,
                    session_id,
                    {
                        "jsonrpc": "2.0",
                        "id": rpc_id,
                        "result": {
                            "userAgent": "Mirror X Mobile Mock Host",
                            "codexHome": r"C:\Users\Administrator\.codex",
                            "platformFamily": "windows",
                            "platformOs": "windows",
                        },
                    },
                )
                continue

            if method == "thread/list":
                if args.fail_all_thread_lists or (
                    params.get("useStateDbOnly") and fast_thread_list_failures > 0
                ):
                    if params.get("useStateDbOnly") and fast_thread_list_failures > 0:
                        fast_thread_list_failures -= 1
                    send_rpc(
                        ws,
                        enc_key,
                        session_id,
                        {
                            "jsonrpc": "2.0",
                            "id": rpc_id,
                            "error": {
                                "code": -32000,
                                "message": "mock thread index unavailable",
                            },
                        },
                    )
                    continue
                send_rpc(
                    ws,
                    enc_key,
                    session_id,
                    {
                        "jsonrpc": "2.0",
                        "id": rpc_id,
                        "result": {
                            "data": state.list_threads(),
                            "nextCursor": iso_now(),
                            "backwardsCursor": iso_now(),
                        },
                    },
                )
                continue

            if method == "thread/read":
                thread_id = params.get("threadId", "")
                thread = state.threads.get(thread_id)
                if thread:
                    payload = dict(thread)
                    payload["turns"] = state.turns_for(thread_id) if params.get("includeTurns") else []
                    send_rpc(
                        ws,
                        enc_key,
                        session_id,
                        {
                            "jsonrpc": "2.0",
                            "id": rpc_id,
                            "result": {"thread": payload},
                        },
                    )
                else:
                    send_rpc(
                        ws,
                        enc_key,
                        session_id,
                        {
                            "jsonrpc": "2.0",
                            "id": rpc_id,
                            "error": {"code": -32000, "message": "thread not found"},
                        },
                    )
                continue

            if method == "thread/resume":
                if args.resume_delay_ms > 0:
                    time.sleep(args.resume_delay_ms / 1000)
                thread_id = params.get("threadId", "")
                thread = state.threads.get(thread_id)
                if thread and thread.get("occupied"):
                    send_rpc(
                        ws,
                        enc_key,
                        session_id,
                        {
                            "jsonrpc": "2.0",
                            "id": rpc_id,
                            "error": {
                                "code": -32600,
                                "message": f"thread {thread_id} already has an active writer",
                            },
                        },
                    )
                    continue
                send_rpc(
                    ws,
                    enc_key,
                    session_id,
                    {
                        "jsonrpc": "2.0",
                        "id": rpc_id,
                        "result": {
                            "thread": {
                                **(thread or {"id": thread_id, "cwd": state.project_cwd}),
                                "turns": [],
                            }
                        },
                    },
                )
                continue

            if method == "thread/fork":
                source_id = params.get("threadId", "")
                source = state.threads.get(source_id)
                if not source:
                    send_rpc(
                        ws,
                        enc_key,
                        session_id,
                        {
                            "jsonrpc": "2.0",
                            "id": rpc_id,
                            "error": {"code": -32000, "message": "thread not found"},
                        },
                    )
                    continue
                forked = state.start_thread(source.get("cwd"))
                with state.lock:
                    target = state.threads[forked["id"]]
                    target["preview"] = source["preview"]
                    target["turns"] = json.loads(json.dumps(source["turns"]))
                    target["materialized"] = True
                    target["forkedFromId"] = source_id
                    target["status"] = {"type": "idle"}
                    forked = dict(target)
                    forked["turns"] = []
                send_rpc(
                    ws,
                    enc_key,
                    session_id,
                    {"jsonrpc": "2.0", "id": rpc_id, "result": {"thread": forked}},
                )
                continue

            if method == "thread/turns/list":
                thread_id = params.get("threadId", "")
                thread = state.threads.get(thread_id)
                if thread and thread.get("materialized"):
                    turns = state.turns_for(thread_id)
                    if params.get("sortDirection") == "desc":
                        turns.reverse()
                    send_rpc(
                        ws,
                        enc_key,
                        session_id,
                        {
                            "jsonrpc": "2.0",
                            "id": rpc_id,
                            "result": {"data": turns},
                        },
                    )
                else:
                    send_rpc(
                        ws,
                        enc_key,
                        session_id,
                        {
                            "jsonrpc": "2.0",
                            "id": rpc_id,
                            "error": {
                                "code": -32000,
                                "message": f"thread {thread_id} is not materialized yet; thread/turns/list is unavailable before first user message",
                            },
                        },
                    )
                continue

            if method == "thread/start":
                thread = state.start_thread(params.get("cwd"))
                send_rpc(
                    ws,
                    enc_key,
                    session_id,
                    {"jsonrpc": "2.0", "id": rpc_id, "result": {"thread": thread}},
                )
                continue

            if method == "fs/readDirectory":
                path = params.get("path", "")
                send_rpc(
                    ws,
                    enc_key,
                    session_id,
                    {
                        "jsonrpc": "2.0",
                        "id": rpc_id,
                        "result": {"entries": state.files.get(path, [])},
                    },
                )
                continue

            if method == "fs/readFile":
                path = params.get("path", "")
                content = state.file_contents.get(path, f"Mock file: {path}\n")
                data = content if isinstance(content, bytes) else content.encode()
                send_rpc(
                    ws,
                    enc_key,
                    session_id,
                    {
                        "jsonrpc": "2.0",
                        "id": rpc_id,
                        "result": {
                            "dataBase64": base64.b64encode(data).decode()
                        },
                    },
                )
                continue

            if method == "turn/steer":
                thread_id = params.get("threadId", "")
                expected_turn_id = params.get("expectedTurnId", "")
                user_text, _ = summarize_user_input(params.get("input", []))
                with state.lock:
                    thread = state.threads.get(thread_id)
                    active_turn = next(
                        (
                            turn
                            for turn in (thread or {}).get("turns", [])
                            if str((turn.get("status") or {}).get("type", "")).lower()
                            in ("active", "inprogress", "in_progress", "running")
                        ),
                        None,
                    )
                    active_turn_id = (active_turn or {}).get("id")
                    if not thread or not thread.get("occupied") or not active_turn_id:
                        error = {
                            "code": -32000,
                            "message": "thread has no active turn to steer",
                        }
                    elif expected_turn_id != active_turn_id:
                        error = {
                            "code": -32000,
                            "message": "expectedTurnId does not match the active turn",
                        }
                    else:
                        active_turn.setdefault("items", []).append(
                            {
                                "id": params.get("clientUserMessageId"),
                                "clientUserMessageId": params.get("clientUserMessageId"),
                                "type": "userMessage",
                                "text": user_text or "（空引导）",
                            }
                        )
                        thread["preview"] = (user_text or "（空引导）")[:90]
                        thread["updatedAt"] = iso_now()
                        thread["recencyAt"] = iso_now()
                        error = None
                if error:
                    send_rpc(
                        ws,
                        enc_key,
                        session_id,
                        {"jsonrpc": "2.0", "id": rpc_id, "error": error},
                    )
                else:
                    send_rpc(
                        ws,
                        enc_key,
                        session_id,
                        {
                            "jsonrpc": "2.0",
                            "id": rpc_id,
                            "result": {"turnId": active_turn_id},
                        },
                    )
                continue

            if method == "turn/start":
                thread_id = params.get("threadId", "")
                user_text, attachment_names = summarize_user_input(params.get("input", []))
                agent_text = (
                    "## 手机端演示结果\n\n"
                    "消息已通过中继返回。\n\n"
                    "- 支持列表\n"
                    "- 支持 `inline code`\n"
                    "- 支持 [镜子AI](https://api.jingziai.club/)\n\n"
                    "- [x] 已完成任务项\n"
                    "- [ ] 待处理任务项\n\n"
                    "*斜体内容* 与 ~~已删除内容~~\n\n"
                    "---\n\n"
                    "> 这是引用格式。\n\n"
                    "| 能力 | 状态 |\n"
                    "| --- | --- |\n"
                    "| 手机续接 | 正常 |\n"
                    "| Markdown | 正常 |\n"
                    "| 转义竖线 | a\\|b |\n\n"
                    "```javascript\nconsole.log('Mirror X Mobile');\n```\n\n"
                    "[！image]"
                )
                turn_id = f"turn-{int(time.time() * 1000)}"
                state.ensure_materialized(
                    thread_id,
                    turn_id,
                    user_text or "（空消息）",
                    agent_text,
                    include_collab=args.collab_agent_demo,
                )
                with state.lock:
                    state.threads[thread_id]["occupied"] = True
                    state.threads[thread_id]["status"] = {"type": "active"}
                send_rpc(ws, enc_key, session_id, {"jsonrpc": "2.0", "id": rpc_id, "result": {}})
                send_notification(
                    ws,
                    enc_key,
                    session_id,
                    "turn/started",
                    {"threadId": thread_id, "turn": {"id": turn_id, "status": "inProgress"}},
                )
                time.sleep(0.2)
                send_notification(
                    ws,
                    enc_key,
                    session_id,
                    "item/started",
                    {
                        "threadId": thread_id,
                        "turnId": turn_id,
                        "item": {
                            "id": f"command-{turn_id}",
                            "type": "commandExecution",
                            "command": "cargo test -p codex-plus-mobile-relay",
                            "status": "inProgress",
                        },
                    },
                )
                time.sleep(0.1)
                send_notification(
                    ws,
                    enc_key,
                    session_id,
                    "item/completed",
                    {
                        "threadId": thread_id,
                        "turnId": turn_id,
                        "item": {
                            "id": f"command-{turn_id}",
                            "type": "commandExecution",
                            "command": "cargo test -p codex-plus-mobile-relay",
                            "status": "completed",
                        },
                    },
                )
                commentary_id = f"commentary-{turn_id}"
                send_notification(
                    ws,
                    enc_key,
                    session_id,
                    "item/started",
                    {
                        "threadId": thread_id,
                        "turnId": turn_id,
                        "item": {
                            "id": commentary_id,
                            "type": "agentMessage",
                            "phase": "commentary",
                            "text": "",
                        },
                    },
                )
                send_notification(
                    ws,
                    enc_key,
                    session_id,
                    "item/agentMessage/delta",
                    {
                        "threadId": thread_id,
                        "turnId": turn_id,
                        "itemId": commentary_id,
                        "delta": "正在核对同步链路与移动端渲染。",
                    },
                )
                send_notification(
                    ws,
                    enc_key,
                    session_id,
                    "item/completed",
                    {
                        "threadId": thread_id,
                        "turnId": turn_id,
                        "item": {
                            "id": commentary_id,
                            "type": "agentMessage",
                            "phase": "commentary",
                            "text": "正在核对同步链路与移动端渲染。",
                        },
                    },
                )
                reasoning_id = f"reasoning-{turn_id}"
                send_notification(
                    ws,
                    enc_key,
                    session_id,
                    "item/started",
                    {
                        "threadId": thread_id,
                        "turnId": turn_id,
                        "item": {
                            "id": reasoning_id,
                            "type": "reasoning",
                            "summary": [],
                            "content": [],
                        },
                    },
                )
                send_notification(
                    ws,
                    enc_key,
                    session_id,
                    "item/reasoning/summaryTextDelta",
                    {
                        "threadId": thread_id,
                        "turnId": turn_id,
                        "itemId": reasoning_id,
                        "summaryIndex": 0,
                        "delta": "历史快照已加载，增量事件继续补齐。",
                    },
                )
                send_notification(
                    ws,
                    enc_key,
                    session_id,
                    "item/completed",
                    {
                        "threadId": thread_id,
                        "turnId": turn_id,
                        "item": {
                            "id": reasoning_id,
                            "type": "reasoning",
                            "summary": ["历史快照已加载，增量事件继续补齐。"],
                            "content": [],
                        },
                    },
                )
                if args.collab_agent_demo:
                    collab_id = f"collab-{turn_id}"
                    send_notification(
                        ws,
                        enc_key,
                        session_id,
                        "item/started",
                        {
                            "threadId": thread_id,
                            "turnId": turn_id,
                            "item": {
                                "id": collab_id,
                                "type": "collabAgentToolCall",
                                "taskName": "核对手机端长任务状态",
                                "status": "inProgress",
                            },
                        },
                    )
                    time.sleep(0.1)
                    send_notification(
                        ws,
                        enc_key,
                        session_id,
                        "item/completed",
                        {
                            "threadId": thread_id,
                            "turnId": turn_id,
                            "item": {
                                "id": collab_id,
                                "type": "collabAgentToolCall",
                                "taskName": "核对手机端长任务状态",
                                "status": "completed",
                            },
                        },
                    )
                if args.turn_response_delay_ms > 0:
                    time.sleep(args.turn_response_delay_ms / 1000)
                agent_item_id = f"agent-{turn_id}"
                send_notification(
                    ws,
                    enc_key,
                    session_id,
                    "item/started",
                    {
                        "threadId": thread_id,
                        "turnId": turn_id,
                        "item": {
                            "id": agent_item_id,
                            "type": "agentMessage",
                            "phase": "final_answer",
                            "text": "",
                        },
                    },
                )
                midpoint = max(1, len(agent_text) // 2)
                for chunk in (agent_text[:midpoint], agent_text[midpoint:]):
                    send_notification(
                        ws,
                        enc_key,
                        session_id,
                        "item/agentMessage/delta",
                        {
                            "threadId": thread_id,
                            "turnId": turn_id,
                            "itemId": agent_item_id,
                            "delta": chunk,
                        },
                    )
                    time.sleep(max(0, args.turn_chunk_delay_ms) / 1000)
                send_notification(
                    ws,
                    enc_key,
                    session_id,
                    "item/completed",
                    {
                        "threadId": thread_id,
                        "turnId": turn_id,
                        "item": {
                            "id": agent_item_id,
                            "type": "agentMessage",
                            "phase": "final_answer",
                            "text": agent_text,
                        },
                    },
                )
                send_notification(
                    ws,
                    enc_key,
                    session_id,
                    "turn/completed",
                    {
                        "threadId": thread_id,
                        "turn": {"id": turn_id, "status": "completed"},
                    },
                )
                with state.lock:
                    state.threads[thread_id]["occupied"] = False
                    state.threads[thread_id]["status"] = {"type": "idle"}
                    for turn in state.threads[thread_id]["turns"]:
                        if turn.get("id") == turn_id:
                            turn["status"] = {"type": "completed"}
                continue

            if method == "turn/interrupt":
                send_rpc(ws, enc_key, session_id, {"jsonrpc": "2.0", "id": rpc_id, "result": {}})
                thread_id = params.get("threadId", "")
                turn_id = params.get("turnId") or "demo-active-turn"
                with state.lock:
                    if thread_id in state.threads:
                        state.threads[thread_id]["occupied"] = False
                        state.threads[thread_id]["status"] = {"type": "idle"}
                        for turn in state.threads[thread_id]["turns"]:
                            if turn.get("id") == turn_id:
                                turn["status"] = {"type": "interrupted"}
                send_notification(
                    ws,
                    enc_key,
                    session_id,
                    "turn/completed",
                    {
                        "threadId": thread_id,
                        "turn": {"id": turn_id, "status": "interrupted"},
                    },
                )
                continue

            send_rpc(
                ws,
                enc_key,
                session_id,
                {
                    "jsonrpc": "2.0",
                    "id": rpc_id,
                    "error": {"code": -32601, "message": f"unsupported mock method: {method}"},
                },
            )

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
