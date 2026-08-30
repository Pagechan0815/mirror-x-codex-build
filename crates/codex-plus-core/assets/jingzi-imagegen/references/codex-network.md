# Network and sandbox notes

The CLI needs outbound HTTPS access to `api.jingziai.club`.

If a request fails before reaching the API:

1. Confirm `scripts/configure.py --status` succeeds.
2. Confirm the active Python environment has the `openai` package.
3. Confirm `https://api.jingziai.club/api/status` is reachable.
4. Retry one image at `1024x1024` without optional controls.
5. Surface the sanitized error and request ID. Never print the API key.

Generation can take several minutes. Do not use a short shell timeout.
