# Scroll-Film Studio — field guide

How to use this skill to build a genuinely cinematic website, and how to choose between its
two lanes. Everything below comes from real shipped builds (including
[openflow.computer](https://openflow.computer)), not theory.

## What you get

One unbroken cinematic shot as your homepage: the visitor scrolls, the film scrubs —
a camera journey you design — and the last frame dissolves into a normal content page
(features, pricing, downloads, docs). Copy appears as scroll-synced "beats" over the film.
All text stays real HTML: selectable, indexable, never baked into footage.

## Quick start

1. Install the skill (see [`../README.md`](../README.md)).
2. In Claude Code, run `/scroll-film-studio`.
3. Answer the interview: what you're building, the journey (where the camera starts and
   ends), which lane, what sits below the film, where it deploys. Every creative question
   has a "you decide" escape hatch — Claude art-directs anything you leave open.
4. Pick one of the 2–3 pitched concepts (each is a concrete scroll-through walkthrough).
5. Approve the costs _before_ anything is spent (Lane B only), watch the draft, approve
   the master. Claude builds, verifies with a real headless-Chrome harness, and hands you
   a finished site.

## The two lanes

**Lane A — Pure code (GSAP + Lenis).** The "film" is scroll-driven motion: pinned scenes,
parallax, clip-path reveals, char-split headline reveals, horizontal runs. No accounts,
no cost, works offline.

**Lane B — Generated footage (Higgsfield Seedance or any start-image image-to-video
engine).** A real 20–30s film, generated as chained clips (each clip starts from the
literal last frame of the previous one), extracted to JPEG frames, scrubbed on a canvas.
This is the signature look — openflow.computer is Lane B.

### Decision table

| Factor                   | Lane A (GSAP)                   | Lane B (footage)                                                       |
| ------------------------ | ------------------------------- | ---------------------------------------------------------------------- |
| **Cash cost**            | $0                              | Engine credits (real numbers below)                                    |
| **Page weight**          | tens of KB of JS                | ~11–17 MB of frames (≈2.5 MB to interactive with the two-phase loader) |
| **Time to first scroll** | instant                         | ~1–3 s on fast connections (loader with progress bar)                  |
| **SEO**                  | unaffected                      | unaffected — copy is HTML, metadata untouched; see notes below         |
| **Look**                 | polished, motion-design         | cinematic, one-take film — the "how did they do that" look             |
| **Mobile data**          | negligible                      | meaningful; consider a reduced mobile frame set                        |
| **Accessibility**        | honors `prefers-reduced-motion` | honors it too (static final frame + full content)                      |
| **Iteration cost**       | free, edit code                 | re-skins/copy free; re-shooting chapters costs credits                 |

**Choose Lane A when:** budget is zero, the audience is data-constrained or docs-focused,
you need the absolute lightest page, or the brand story doesn't need literal imagery.

**Choose Lane B when:** it's a product launch or brand moment, you want a hero that
nobody else has, and a one-time ~$5–15 of engine credits is acceptable.

### Lane B cost, measured (Higgsfield Seedance 2.0, July 2026)

Per 5-second clip, audio off:

| Tier                                                            | Credits |
| --------------------------------------------------------------- | ------- |
| 480p / fast (drafting)                                          | 7.5     |
| 720p / std (delivery-quality: matches the 1280px frame payload) | 22.5    |
| 1080p / std (max quality)                                       | 45      |
| Opening keyframe (Nano Banana Pro image)                        | 2       |

A full 5-chapter film: **~40 credits to draft the whole chain cheap**, then
**112.5 (720p) or 225 (1080p) to master** only after you approve the draft. Two levers
matter more than anything: **audio off** (audio silently ~3×'s the bill) and **draft
first, master once**. ~15% of jobs fail server-side unbilled — retries are free.
The skill quotes the total before spending and shows the balance receipt after.
Practical note: 720p/std is usually the right master tier — the page scrubs 1280px-wide
frames, so 1080p only buys slight supersampling sharpness.

## SEO & performance notes (both lanes)

- **All copy is HTML.** Headlines, CTAs, features — never baked into footage. Search
  engines see a normal page; the film is presentation only.
- Keep `<title>`, meta description, canonical, OG tags, JSON-LD exactly as you would on
  any site. The skill preserves existing metadata when retrofitting a live site.
- **Core Web Vitals:** the hero beat (wordmark + tagline) is HTML visible at scroll 0, so
  LCP is text/first-frame, not the full film. Lane B's two-phase loader unlocks the page
  on a sparse ~2.5 MB frame ladder and streams the rest behind.
- Verification is built in: a puppeteer harness screenshots every beat and runs a jank
  test (per-frame rAF deltas — judge p95/max, never average FPS; target max < 50 ms).

## Hard-won engine rules (violate these and you will ship bugs)

These are encoded in `references/engine.md` / `references/playbook.md`; the two that cost
us real debugging hours:

1. **Never scrub a `<video>` tag.** Seek latency stutters. Canvas + pre-extracted JPEG
   frames only.
2. **Never use `createImageBitmap` in the scrub engine.** Its decode worker races the
   canvas raster on macOS Chrome and _crashes the browser tab_ on fast upward scroll
   reversals. Warm frames with `img.decode()`, copy them to pooled offscreen canvases
   (throttled to one copy per tick), and blit canvas→canvas. Also: `overflow-anchor: none`
   on the page, snap the playhead on giant jumps, and skip one tick after a >2-viewport
   scroll delta.
3. **Junction gates are measured, never eyeballed.** Every clip-to-clip seam is
   SSIM-scored; a structural mismatch means regenerate — dissolves to hide a bad seam are
   forbidden because a scrubbing user can park on it.

## What a Lane B build produces

```
your-site/
  index.html            # film hero + your content sections
  src/film.js           # the scrub engine (vanilla JS, no libraries)
  src/film.css
  public/film/f_0001.jpg … f_0300.jpg
working-dir/
  STORYBOARD.md         # the 5-chapter camera arc + prompts
  keyframe.png          # Nano Banana opening frame
  draft/  master/       # chained clips + junction comparisons
  frames-master/        # extracted scrub frames + seam colour
```

The footage is reusable: one film can power multiple page designs — frames are the cost,
re-skins are free.

## Using another video engine

Higgsfield is the reference (scripts included). Kie.ai, fal, Replicate, or anything that
accepts a `--start-image` works: keep the exact chain contract — generate → wait →
download → extract last frame → SSIM junction gate — and swap only the generate/download
calls in `scripts/chain-step.sh`.
