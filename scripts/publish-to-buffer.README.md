# Publish growth-loop drafts to Buffer

`scripts/publish-to-buffer.mjs` reads a daily growth-loop artifact, uploads referenced images to Cloudflare R2 with `wrangler`, and creates Buffer drafts through Buffer's GraphQL API. It never schedules or queues live posts; the script hard-codes `saveToDraft: true`.

## Prerequisites

- Node 20 or newer.
- `wrangler` installed and authenticated.
- Cloudflare R2 bucket created.
- R2 public access enabled with a public base URL such as `https://pub-<hash>.r2.dev`.
- Buffer X channel connected, with the Buffer channel id available.

## Environment

```sh
export BUFFER_ACCESS_TOKEN=<your-token-here>
export R2_PUBLIC_BASE_URL=https://pub-<hash>.r2.dev
export R2_BUCKET=spur-growth-loop
```

`R2_BUCKET` defaults to `spur-growth-loop` when omitted. Do not put the Buffer token in files, shell history examples, issue comments, or fixtures.

The script refuses to run when `NODE_OPTIONS` contains `--inspect`, because a debugger can expose environment variables.

## Usage

```sh
scripts/publish-to-buffer.mjs --channel-id <your-buffer-x-channel-id>
scripts/publish-to-buffer.mjs --artifact resource/growth-loop/2026-05-20.md --channel-id <your-buffer-x-channel-id>
scripts/publish-to-buffer.mjs --dry-run --artifact resource/growth-loop/2026-05-20.md --channel-id <your-buffer-x-channel-id>
```

`--dry-run` reads only the artifact and prints the parsed posts, planned R2 uploads, and GraphQL variables. It does not upload images, call Buffer, or write the artifact.

## Buffer smoke test

This creates a draft in Buffer if the token and channel id are valid.

```sh
curl -sS -X POST 'https://api.buffer.com' \
  -H "Authorization: Bearer $BUFFER_ACCESS_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"query":"mutation SmokeTest($input: CreatePostInput!) { createPost(input: $input) { __typename ... on PostActionSuccess { post { id text } } ... on MutationError { message } } }","variables":{"input":{"text":"SPUR Buffer API smoke test draft","channelId":"<your-buffer-x-channel-id>","schedulingType":"automatic","mode":"addToQueue","saveToDraft":true,"assets":[]}}}'
```

Buffer draft docs: `https://developers.buffer.com/examples/create-draft-post.html`.
Buffer X thread metadata docs: `https://developers.buffer.com/types/TwitterPostMetadataInput.html`.
Buffer image asset docs: `https://developers.buffer.com/examples/create-image-post.html`.

## Failure modes

| Symptom | Exit | Cause | Action |
|---|---:|---|---|
| `BUFFER_ACCESS_TOKEN not set` | 1 | Missing Buffer API token | Export `BUFFER_ACCESS_TOKEN=<your-token-here>`. |
| `R2_PUBLIC_BASE_URL not set` | 1 | Missing public R2 URL | Export the `https://pub-<hash>.r2.dev` URL. |
| `Refusing to run with NODE_OPTIONS containing --inspect` | 1 | Debugger exposure risk | Run from a shell without inspector flags. |
| Parse error | 1 | Artifact does not contain `## Drafts — X` or usable X drafts | Fix the markdown artifact shape. |
| Unsupported image extension | 1 | Image path is not `.png`, `.jpg`, `.jpeg`, or `.webp` | Use a supported image type. |
| R2 upload failed | 2 | `wrangler r2 object put` failed | Check Wrangler auth, bucket name, and local image paths. |
| Buffer API error | 3 | GraphQL HTTP, schema, auth, or mutation error | Check token, channel id, and Buffer's response message. |

## Exit codes

- `0`: Success.
- `1`: Missing config or parse/validation error.
- `2`: R2 upload failed.
- `3`: Buffer API error.
