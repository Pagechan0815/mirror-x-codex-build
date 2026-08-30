---
name: jingzi-imagegen
description: Generate and save images with 镜子AI gpt-image-2. Use automatically when the user asks to create, draw, render, or generate an image, illustration, poster, cover, mockup, product visual, or website asset.
---

# 镜子AI 生图

Handle the complete image request for the user. The user only describes the desired result; never ask them to run commands or understand the Skill implementation.

## Generate

- Turn the request into a clear image prompt. Read `references/prompting.md` only for complex composition or typography.
- Use the Skill's registered generator internally with `gpt-image-2`.
- Save one image under the current project's `output/imagegen/` by default. Preserve a requested aspect ratio and create multiple variants only when requested.
- Inspect the file, then show the image and its absolute path. Do not expose internal commands, helper paths, credential paths, or temporary URLs.

## Image Key

- The Key is independent from Codex model/provider configuration and must never be written to Codex `config.toml`.
- If a valid Key is already registered, use it without asking again.
- If the user directly provides an Image Key, read `references/key-registration.md`, register it without echoing it, then continue the original image request.
- If no Key is available, ask for the 镜子AI Image Key once. Do not send a generation request until registration succeeds.

## Boundaries

- Image generation is supported; live image editing and native transparent backgrounds are not.
- Billing follows the current 镜子AI `gpt-image-2` price.
- Never claim success until the output exists and has been inspected.
