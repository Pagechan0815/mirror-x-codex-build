#!/usr/bin/env python3
"""Configure the local 镜子AI image-generation credential."""

from __future__ import annotations

import argparse
import getpass
import json
import os
from pathlib import Path
import sys

DEFAULT_BASE_URL = "https://api.jingziai.club/v1"


def config_path() -> Path:
    override = os.getenv("JINGZI_IMAGEGEN_CONFIG")
    if override:
        return Path(override).expanduser()
    return Path.home() / ".config" / "jingzi-imagegen" / "config.json"


def mask_key(value: str) -> str:
    value = value.strip()
    if len(value) <= 10:
        return "*" * len(value)
    return f"{value[:5]}...{value[-4:]}"


def load_config() -> dict[str, str]:
    path = config_path()
    if not path.exists():
        return {}
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise RuntimeError(f"配置文件无法读取：{path}: {exc}") from exc
    return value if isinstance(value, dict) else {}


def write_config(api_key: str, base_url: str) -> Path:
    path = config_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    temp_path = path.with_suffix(path.suffix + ".tmp")
    temp_path.write_text(
        json.dumps(
            {"api_key": api_key.strip(), "base_url": base_url.rstrip("/")},
            ensure_ascii=False,
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    try:
        os.chmod(temp_path, 0o600)
    except OSError:
        pass
    temp_path.replace(path)
    try:
        os.chmod(path, 0o600)
    except OSError:
        pass
    return path


def main() -> int:
    parser = argparse.ArgumentParser(description="配置镜子AI Imagegen Skill")
    parser.add_argument("--api-key", help="镜子AI API Key；不传则隐藏输入")
    parser.add_argument("--base-url", default=DEFAULT_BASE_URL)
    parser.add_argument("--status", action="store_true")
    parser.add_argument("--remove", action="store_true")
    args = parser.parse_args()

    path = config_path()
    if args.remove:
        if path.exists():
            path.unlink()
        print(f"已删除配置：{path}")
        return 0

    if args.status:
        env_key = os.getenv("JINGZI_API_KEY") or os.getenv("MIRROR_API_KEY")
        if env_key:
            print(f"状态：已配置环境变量，Key={mask_key(env_key)}")
            print(f"Base URL：{os.getenv('JINGZI_BASE_URL', DEFAULT_BASE_URL)}")
            return 0
        config = load_config()
        key = str(config.get("api_key", "")).strip()
        if not key:
            print(f"状态：未配置\n配置路径：{path}")
            return 1
        print(f"状态：已配置，Key={mask_key(key)}")
        print(f"Base URL：{config.get('base_url', DEFAULT_BASE_URL)}")
        print(f"配置路径：{path}")
        return 0

    api_key = (args.api_key or "").strip()
    if not api_key:
        api_key = getpass.getpass("请输入镜子AI API Key（输入内容不会显示）：").strip()
    if not api_key:
        print("错误：API Key 不能为空。", file=sys.stderr)
        return 2

    saved = write_config(api_key, args.base_url)
    print(f"配置完成：{saved}")
    print(f"Key：{mask_key(api_key)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
