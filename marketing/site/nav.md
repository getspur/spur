# SPUR Navigation — Header + Footer Copy & IA Rationale

*2026-05-20. Phase-3 deliverable (`mkt.web.site-arch`), companion to `marketing/site/sitemap.md`. Grounded in `marketing/product-marketing.md` V1.3 brand voice (`product-marketing.md:169-173`), `marketing/messaging/positioning.md` hero candidates, and developer-tool convention (Tailscale, Linear, Resend, Fly.io, Anthropic Console).*

---

## IA rationale — why this nav, not a SaaS nav

**Decision: developer-tool nav, not SaaS marketing nav.**

A SaaS marketing nav would surface "Solutions / Industries / Resources / Customers" mega-menus. SPUR's audience (Sr/Staff eng, tmux-native, `product-marketing.md:25-27`) reads those as buzzword surfaces and skips them. Sites SPUR's audience trusts — Tailscale, Linear, Resend, Anthropic Console, Sentry, PostHog — all run flat 5-7-item header nav with a single CTA on the right. We mirror that pattern.

Five governing principles:

1. **5 nav items + 1 CTA + 1 sign-in.** Capped at 7 affordances total per header rule (`marketing-site-architecture/SKILL.md:178`).
2. **No mega-menu.** Comparisons (`/vs/*`) is the only dropdown, and it lists three items — fits a simple dropdown, never warrants a mega-panel.
3. **No "Sign in with GitHub."** Per task constraint and `product-marketing.md:10` (proprietary, no public repo). Sign-in is email magic-link.
4. **No "GitHub" star/link.** Per task constraint and `product-marketing.md:10`. The credibility token in the header is the live install command, not a star count.
5. **Footer carries discovery, legal, and trust.** Blog, Changelog, Status, EULA, Privacy, Security live there — present but not load-bearing for the launch funnel.

---

## Header navigation

### Layout (left → right)

```
[SPUR logo]   Pricing   Quickstart   Comparisons ▾   Docs   Changelog          Sign in   [Install]
```

- **Logo** links to `/` (homepage; resets `?hero=` if present).
- **5 nav items** in the middle band, ordered by funnel proximity.
- **Sign in** is a low-emphasis text link (right side); reveals `/account` magic-link form when clicked.
- **Install** is the single high-emphasis CTA (filled button), opens an inline copyable install command + links to `/quickstart`.

### Header items — copy, target, rationale

| Order | Label | Target URL | Style | Rationale |
|---|---|---|---|---|
| 1 | **Pricing** | `/pricing` | Text link | Highest revenue-intent click; convention puts it first or last — first is more common on developer-tool sites (Tailscale, Linear, Resend all lead with Pricing). |
| 2 | **Quickstart** | `/quickstart` | Text link | The product is "install + run" — exposing the install path in nav reinforces "this is a real CLI, not a SaaS waitlist." |
| 3 | **Comparisons** ▾ | `/vs` (dropdown) | Dropdown | Three peers per `positioning.md:90-108`. Label chosen over "vs" (cryptic in a nav), "Alternatives" (false framing — we're a peer, not a replacement), and "Why SPUR" (vague). |
| 4 | **Docs** | `/docs` | Text link | Developer-tool table stakes. Placeholder at launch (see `sitemap.md`) but the slot must exist for `spur --help` to deep-link into. |
| 5 | **Changelog** | `/changelog` | Text link | Borrowed from Tailscale / Linear / Resend convention — signals an alive, shipping product. Especially important for a pre-launch tool with no testimonial section. |

#### "Comparisons" dropdown contents

```
Comparisons ▾
├── vs Claude Code      → /vs/claude-code
├── vs Devin            → /vs/devin
└── vs Cursor           → /vs/cursor
```

Order = search intent + funnel value (`positioning.md:106, 94, 98` respectively). No "vs Aider" yet — per `positioning.md:102-104`, Aider users are *warmest leads*, framed as "add SPUR above Aider" rather than head-to-head; a `/vs/aider` page would pick a fight we don't want.

### CTA + Sign-in copy

| Element | Copy | Hover / focus state |
|---|---|---|
| **Install button** (primary CTA, right-most) | `curl ⇩ Install` | On hover: reveals tooltip with full `curl -sSL getspur.dev/install.sh \| sh` and a copy button. Tap on mobile: opens `/quickstart`. |
| **Sign in link** (secondary, left of CTA) | `Sign in` | Opens a slide-down form: single email field → "Email me a sign-in link". No password. |

**Why "Install" as CTA copy, not "Get started" / "Try free" / "Sign up":**
- "Get started" is brochure-ware (`product-marketing.md:142-147` brand-voice avoid list).
- "Try free" implies trial mechanics — SPUR Community is genuinely free, no trial (`product-marketing.md:104`).
- "Sign up" is wrong — Community needs no signup (`product-marketing.md:204`).
- "Install" matches the actual user action and the `curl | sh` distribution choice (`product-marketing.md:10`).

### Mobile (< 768px) header

```
[SPUR logo]                                          [Install]   [≡]
```

Tap `≡` → slide-in panel with full nav list (same 5 items + dropdown items flattened) + Sign in. Install button stays exposed because it's the primary funnel action.

### Authenticated-state header (post sign-in)

When a user is signed in to `/account/*`:

```
[SPUR logo]   Pricing   Quickstart   Comparisons ▾   Docs   Changelog          Account ▾   [Install]
```

`Sign in` → `Account ▾` dropdown:
```
Account ▾
├── License        → /account/license
├── Billing        → /account/billing
├── Team           → /account/team
└── Sign out
```

---

## Footer navigation

### Layout (4 columns + bottom band)

```
┌─────────────────┬─────────────────┬─────────────────┬─────────────────┐
│ Product         │ Compare         │ Resources       │ Company         │
│ ─────────────── │ ─────────────── │ ─────────────── │ ─────────────── │
│ Pricing         │ vs Claude Code  │ Docs            │ Feedback        │
│ Quickstart      │ vs Devin        │ Blog            │ Security        │
│ Install         │ vs Cursor       │ Changelog       │ EULA            │
│ Account         │                 │ Status          │ Privacy         │
└─────────────────┴─────────────────┴─────────────────┴─────────────────┘
─────────────────────────────────────────────────────────────────────────
SPUR © 2026 · Built for developers who already pay for three coding agents.
```

### Footer columns — copy and rationale

#### Column 1 — Product

| Label | Target | Why here |
|---|---|---|
| Pricing | `/pricing` | Repeats header — load-bearing for conversion |
| Quickstart | `/quickstart` | Repeats header |
| Install | (anchor: opens install tooltip) | Explicit footer entry for users who scrolled past the header CTA |
| Account | `/account` | Sign-in entry from the footer; same magic-link form |

#### Column 2 — Compare

| Label | Target | Why here |
|---|---|---|
| vs Claude Code | `/vs/claude-code` | Surfaces dropdown contents flat in footer (SEO + accessibility) |
| vs Devin | `/vs/devin` | " |
| vs Cursor | `/vs/cursor` | " |

#### Column 3 — Resources

| Label | Target | Why here |
|---|---|---|
| Docs | `/docs` | Repeats header |
| Blog | `/blog` | First exposure outside header; Phase-5 will surface posts inline |
| Changelog | `/changelog` | Repeats header (also surfaced via in-product update banner) |
| Status | `https://status.getspur.dev` | Developer-tool convention; external subdomain |

#### Column 4 — Company

| Label | Target | Why here |
|---|---|---|
| Feedback | `/feedback` | Target of `spur feedback` (`product-marketing.md:183`); also a no-install entry for VOC |
| Security | `/security` | Signed-binary checksums, vuln disclosure — credibility for a proprietary binary |
| EULA | `/eula` | Legal — binding for Community tier |
| Privacy | `/privacy` | Legal — required given telemetry + email + license dashboard (see `sitemap.md` summary §a) |

**Note: no "About" or "Careers" links at launch.** Pre-launch single-product company; About reads as filler. Add post-launch if a real team page is published.

### Footer bottom band

> **SPUR © 2026 · Built for developers who already pay for three coding agents.**

The strapline restates the audience (`product-marketing.md:25-26`) in a way that filters out anti-personas (`product-marketing.md:107-111`) on first scroll. Honest about positioning. No copyright fight to pick over a 2026-only year string.

---

## Breadcrumbs

Disabled for marketing pages (`/`, `/pricing`, `/quickstart`, `/vs/*`). Flat L1 hierarchy doesn't need them, and they read as "this should have been a SaaS deep-link maze" — wrong signal.

Enabled for two sections only:

| Section | Format | Example |
|---|---|---|
| `/blog/{slug}` | `Home > Blog > {title}` | `Home > Blog > Why your Claude Code session dies at hour 1` |
| `/account/*` | `Account > {subsection}` | `Account > Billing` |
| `/docs/**/*` (post-launch) | `Docs > {section} > {page}` | `Docs > Concepts > Worktree per worker` |

Implementation note: breadcrumbs ship as `BreadcrumbList` JSON-LD (handled by `mkt.web.schema-jsonld`, `CAMPAIGN_PLAN.md:70`).

---

## Behavior + interaction details

### Hero A/B/C and the canonical URL

`/`, `/?hero=cost`, `/?hero=tower`, `/?hero=tmux` all render homepages with different hero blocks (Hero A/B/C per `positioning.md:60-86`) but share the same `<link rel="canonical" href="https://getspur.dev/">`. The nav and footer do not change between variants. Hero-variant choice is tracked as an analytics dimension, not a URL change.

### Install button as the canonical CTA

Every page below `/`, `/pricing`, `/quickstart`, and `/vs/*` repeats the **Install** CTA in the header. Pricing additionally exposes a **Buy Pro** button next to Install in the right band — only on `/pricing`. This is the single point where two CTAs coexist; everywhere else the CTA singleton rule holds.

### "Sign in" → magic link, never password

`/account` accepts an email address, mails a one-time signed link valid for 15 minutes, and lands the user authenticated. No password, no OAuth — matches the proprietary + email-license model (`product-marketing.md:11`).

### External links — when to mark `target="_blank"`

Only one external link in the nav surface: **Status** → `status.getspur.dev`. Open in a new tab so users monitoring an incident don't lose the marketing surface. All other links stay in-tab; same-origin SPA navigation if implemented.

---

## Accessibility + SEO checklist

- [ ] Logo wraps `<a href="/" aria-label="SPUR — home">`.
- [ ] Install button is `<button>` (opens dialog), not `<a>` — it doesn't navigate to a URL.
- [ ] Comparisons dropdown is keyboard-navigable (arrow keys + Esc to close).
- [ ] Mobile menu toggle has `aria-expanded` and `aria-controls`.
- [ ] All footer columns ship with `<nav aria-label="Footer — Product">` (etc.) for screen readers.
- [ ] No nav item relies on color alone for active-state — use underline or bold.
- [ ] `/changelog` exposes an `<link rel="alternate" type="application/rss+xml">` for RSS readers.
- [ ] `sitemap.xml` excludes `/pro`, `/account/*`, `/feedback`, `/404` (see `sitemap.md` priority table).
- [ ] `robots.txt` allows everything except `/account/*` and `/pro`.

---

## What this nav deliberately omits

| Omitted | Why |
|---|---|
| GitHub link / star button | No public repo (`product-marketing.md:10`, task constraint) |
| "Sign in with GitHub" | Proprietary, email + license signup (task constraint) |
| "Solutions" mega-menu | SaaS pattern; wrong audience |
| "Resources" mega-menu | SaaS pattern; Docs + Blog + Changelog cover the legit jobs flat |
| "Customers" / "Case studies" | No testimonials yet (`product-marketing.md:181-182` flags this as launch-blocker; revisit when 3-5 quotes exist) |
| "About" / "Team" / "Careers" | Pre-launch single-product company; filler at this stage |
| "Login" / "Log in" | Sign-in is via magic-link, label is "Sign in" — never "Log in" (Apple-style; matches developer-tool convention) |
| "Free trial" / "Get started for free" | Community tier is free, no trial mechanic (`product-marketing.md:104`) |
| Language switcher | English-only at launch |
| Dark / light toggle | Site defaults to dark — matches the audience and the brand-visual direction (`marketing/brand-visual.md`); manual toggle is a Phase-3+ nice-to-have, not a launch blocker |
