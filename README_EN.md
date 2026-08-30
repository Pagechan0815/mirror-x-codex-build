# Mirror X Codex

Mirror X Codex is a Mirror AI connection tool for Codex Desktop. Users enter their Mirror AI key locally, validate the models available to each key, and use those models from the official Codex application.

It does not replace the official Codex client. It applies and restores the local model connection safely.

## Download

Get the current installer from the [latest release](https://github.com/Pagechan0815/mirror-x-codex-build/releases/latest).

| Platform | Installer |
| --- | --- |
| Windows 10/11 x64 | `mirror-x-codex-*-windows-x64-setup.exe` |
| Intel Mac | `mirror-x-codex-*-macos-x64.dmg` |
| Apple Silicon Mac | `mirror-x-codex-*-macos-arm64.dmg` |

The Chinese beginner guide is available at [Mirror X Codex user guide](docs/Mirror-X-Codex-用户安装与使用指南.md).

## What it does

- Checks whether Codex is installed and links to the official installer when needed.
- Supports separate CodexPro (GPT / Grok) and Claude key groups.
- Routes each selected model through its assigned key group.
- Supports mixed API mode and pure API mode.
- Preserves MCP configuration, plugin marketplace settings, and unknown Codex settings.
- Creates a local baseline before first use and can restore the pre-connection state.
- Correctly forwards Codex context-compaction requests, including `/v1/responses/compact`.

## Security

Do not share or commit API keys. Keys belong only in the local Mirror X Codex application.

## Release validation

Each release is built for Windows x64, macOS Intel, and macOS Apple Silicon through GitHub Actions. The app reports local token/context usage; server balance and plan quota remain authoritative in the relay console.
