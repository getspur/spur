# Context Service Hosted UI branding

SPUR brands the **Cognito classic Hosted UI** at `https://auth.context.getspur.dev`
(the page humans see after `spur context auth login` opens the browser).

There is no first-party login HTML in the Lambda. `GET /auth/login` on the API
hostname is only a credential-free **302 redirect facade** to Cognito's
`/oauth2/authorize` (see the hybrid Cognito auth design). Visual work therefore
lives here as Cognito UI customization, not a custom SPA.

## Assets

| File | Purpose |
|---|---|
| `cognito-login.css` | Overrides Cognito `*-customizable` classes with SPUR terminal-dark palette |
| `logo.svg` | Source wordmark (edit this) |
| `logo.png` | Cognito banner image (`filebase64` in Terraform; regenerate from SVG) |
| `preview.html` | Local preview that mirrors Cognito's mobile modal DOM |

Palette follows `marketing/brand-visual.md` (terminal-dark `#0E1116`, amber
signal `#FFB454`, mono type).

## Limits (AWS Cognito)

- CSS ≤ 100 KiB
- Image ≤ 100 KiB (PNG or JPG)
- Only Hosted UI hooks can be restyled — HTML structure is owned by Cognito

## Local preview

```bash
open infra/spur-context-service/hosted-ui/preview.html
# or
python3 -m http.server 8760 --directory infra/spur-context-service/hosted-ui
```

## Regenerate logo PNG

```bash
rsvg-convert -w 360 -h 81 \
  infra/spur-context-service/hosted-ui/logo.svg \
  -o infra/spur-context-service/hosted-ui/logo.png
```

## Deploy

Terraform resource `aws_cognito_user_pool_ui_customization.context_service`
applies pool-wide branding whenever `cognito_auth_enabled` is true. Use the
normal `infra/spur-context-service` plan/apply path.

After apply, force-refresh the browser on the authorize URL (CloudFront may
cache Hosted UI assets briefly).
