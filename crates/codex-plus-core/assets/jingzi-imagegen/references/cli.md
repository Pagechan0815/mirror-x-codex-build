# CLI reference (`scripts/image_gen.py`)

Mirror X Codex installs a small cross-platform helper and platform wrappers with this Skill. The helper routes generation to 镜子AI without Python, Adobe, or an official OpenAI API key.

## Capabilities

- `generate`: create a new image.
- Multiple variants: pass `--n 2` through `--n 10`.

Live calls require network access and an Image Key configured through Mirror X Codex.

## Generate

Windows:

```powershell
& "<skill-dir>\scripts\jingzi-imagegen.cmd" generate `
  --prompt "A cozy alpine cabin at dawn" `
  --size 1024x1024 `
  --out "output\imagegen\alpine-cabin.png"
```

macOS:

```bash
"<skill-dir>/scripts/jingzi-imagegen" generate \
  --prompt "A cozy alpine cabin at dawn" \
  --size 1024x1024 \
  --out output/imagegen/alpine-cabin.png
```

## Controls

- `--prompt`: required image description.
- `--out`: required output path.
- `--size`: aspect-ratio intent, default `1024x1024`.
- `--n`: 1 to 10 variants. Multiple files receive `-1`, `-2`, and so on.
- `--force`: allow replacing existing output files.

Gateway guarantees:

- `prompt`, image count, and aspect-ratio intent are supported.
- Exact pixels and encoded image format are controlled by the current upstream implementation.
- The helper accepts both a temporary `url` and `b64_json`, then writes a local file.

## Output Handling

- Finals: `output/imagegen/`
- Existing files are protected unless `--force` is passed.

## Transparent Output

Native transparency is unavailable. Ask for a flat chroma-key background if later local removal is needed. Do not switch to another model; this gateway exposes only `gpt-image-2`.
