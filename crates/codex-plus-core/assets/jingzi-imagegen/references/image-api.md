# 镜子AI Image API reference

## Endpoint

```text
POST https://api.jingziai.club/v1/images/generations
```

Authentication:

```http
Authorization: Bearer <镜子AI API Key>
Content-Type: application/json
```

Request:

```json
{
  "model": "gpt-image-2",
  "prompt": "A polished silver pocket watch",
  "n": 1,
  "size": "1024x1024"
}
```

Guaranteed controls:

- `prompt`
- `n`
- `size` as aspect-ratio intent

The copied Codex CLI retains official imagegen options for workflow compatibility. The gateway does not guarantee that quality, exact pixels, output format, compression, moderation, background, or style alter the upstream image.

## Response

The gateway can return a temporary site URL:

```json
{
  "data": [
    {
      "url": "https://api.jingziai.club/api/image-temp/..."
    }
  ]
}
```

It can also return `b64_json`. The CLI handles both forms and saves the result locally.

Temporary links are transport links, not permanent project assets.

## Boundaries

- Model: `gpt-image-2` only.
- No live `/v1/images/edits`.
- No masks or reference-image upload.
- No native transparent background.
- No automatic provider or model fallback.
- Billing follows the current 镜子AI pricing page.
