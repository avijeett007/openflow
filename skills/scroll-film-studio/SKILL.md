---
name: scroll-film-studio
description: >-
  Build a genuinely beautiful animated scroll-film website — the whole page is one
  continuous cinematic shot that plays as the visitor scrolls. Runs a short interview,
  pitches 2-3 named concepts, art-directs the world, then builds it from scratch.
  Two lanes: free pure-code GSAP/Lenis motion (zero setup, works for anyone) or a
  cinematic footage film from the user's own image-to-video engine (Higgsfield Seedance
  is the reference; Kie.ai, fal, Replicate or any start-image-capable model works).
  Trigger on "scroll-film", "cinematic scroll site", "scrollytelling website", "build me
  an animated/scroll website", "film-scroll site", "one continuous shot website", or any
  request for a premium scroll-scrubbed animated site. NOT for slide decks / HTML
  explainers or static brochure sites.
---

# Scroll-Film Studio

You build **scroll-film websites**: the hero _is_ the page — one unbroken cinematic
shot that scrubs as the visitor scrolls, then dissolves seamlessly into the content
below. This skill is a **process, not a scaffold** — there are no template pages to
copy. Every site is designed and written from scratch for its brand, guided by the
process below and the technical law in `references/`.

Two ways to make the film:

- **Lane A — Pure-code (default, zero setup):** the "film" is GSAP + Lenis motion —
  pinned scenes, parallax, clip-path reveals, horizontal runs. Costs nothing, needs no
  accounts, works for anyone who downloads this skill.
- **Lane B — Cinematic footage (opt-in):** the film is real generated video, chained
  shot-to-shot and scrubbed on a canvas. Works with **any image-to-video engine that
  accepts a start image** — Higgsfield Seedance 2.0 is the reference implementation
  (scripts included); Kie.ai Seedance/Veo, fal, Replicate etc. follow the same chain
  contract. Needs the user's own account + credits. This is the signature look.

Everyone gets a gorgeous result. Lane A is always available; Lane B unlocks when the
user has a video engine.

---

## THE GOLDEN RULE — design is done by you, the Anthropic model

Every decision that involves **taste** is done by you, the Claude model running this
skill: concepts, art direction, palette, type, layout, motion design, copy, the build
itself (all HTML/CSS/JS), and the final design review. **No other model ever touches
the design space.** If you delegate, delegate only:

- **Mechanical work** → pure shell/code with _no model at all_ (ffmpeg, SSIM scoring,
  frame extraction, verification, deploys).
- **Bounded drafting** → sub-agents _that are also Claude_ (e.g. drafting one chapter's
  video prompt, writing one after-film section). Never route design or code to a
  non-Anthropic model.

This is non-negotiable and is how quality stays high while tokens stay low.

---

## STEP 0 — The interview

Ask these up front (batch them; prefer the host's structured-question UI if available).
**Every creative question has a "you decide" path** — if the user defers, you art-direct
it yourself and keep moving. Never block on a design answer you can make well.

1. **What are we building, and the one-line vibe?**
   Brand/product name, what it is, and the feeling. (e.g. _"VOLTA — an electric race
   team. Aggressive, electric, fast."_)
2. **Brand assets, or should I create the world?**
   Existing logo / colours / fonts / real images — or full creative freedom.
3. **The journey — the one continuous shot, top to bottom.**
   Where the camera starts and where it ends — the _transformation_. (e.g. _"moonlit
   field → into a single bloom → a drop of gold → the bottle."_) Or: "design the arc
   from my brand." **This is the heart of the whole build.**
4. **Real video, or pure motion?** → picks Lane B or Lane A. If unsure or zero-setup,
   default to **Lane A (pure-code)**.
5. **(Lane B only) "Are you using Higgsfield, or something else?"** Ask this
   explicitly. Higgsfield CLI is the reference path (scripts included); Kie.ai, fal,
   Replicate, or any image-to-video model that accepts a start image also works. Then:
   is it installed/authed? How many chapters (clips)? A credit ceiling? — You will
   draft cheap, confirm the cost, and only master in full resolution on their
   approval. If they have no engine, fall back to Lane A.
6. **What comes after the film?** The sections below the scroll (lineup / collection /
   booking / manifesto…), the primary call-to-action, contact + socials.
7. **Where does it go live?** Local only, or publish to _their own_ Vercel.

---

## STEP 1 — Pitch concepts back (before building anything)

From the interview, develop **2–3 named creative concepts** and pitch them. Rules:

- Lead with your **recommended** concept, explicitly marked "(Recommended)".
- Each concept gets a _concrete what-you-actually-see walkthrough_, not a thesis
  one-liner — narrate the scroll: what the visitor sees at the top, what happens as
  they scroll, what each chapter shows, how the film resolves into the content.
  (e.g. _"You open on a moonlit flower field, huge serif wordmark floating over it.
  Scroll: the camera dives into a single bloom… petals part… you're falling through
  gold embers… a drop of liquid gold lands in a pool… pull back — you're inside the
  bottle on black marble. The page then melts into the collection."_)
- Name each concept (a title is half the sell), state the lane it uses, the chapter
  count, and (Lane B) the estimated credits.
- **Optional second-model sparring (if available):** before presenting, check whether a
  second frontier-model CLI exists on the user's machine (e.g. `codex`, `gemini`, or
  similar). If one does, hand it the concepts _as text_ and ask it to (a) attack each
  one — is the journey legible? memorable? feasible in N chapters? — and (b) propose one
  wildcard angle you haven't considered. Fold what survives into your pitch (credit the
  sparring in one line). **This is strategy critique only — the other model never writes
  copy, code, or any design decision; you arbitrate and you author.** If no second model
  is available, skip silently — the skill is fully self-sufficient on Claude alone.
- Let the user pick or blend; if they say "you choose", take the recommended one and go.

Only after a concept is chosen do you build.

---

## STEP 2 — Art-direct the world (you, alone)

Decide and commit: palette (exact hexes), a display+body **type pairing** with real
character (never default system fonts — reach for expressive display faces), a logo
lockup (inline SVG), the motion feel, and the chapter names. Distinct fonts and a
distinct world per brand — never ship two brands that look like the same site. Pull
real brand logos as inline SVG for any named third-party tool (never a hand-drawn
approximation of a real logo).

---

## LANE A — Pure-code (default)

Write a single self-contained HTML page from scratch for this brand. Load GSAP,
ScrollTrigger, and Lenis from CDN (vendor them locally for production). Compose the
film from the motion vocabulary in `references/engine.md` §Pure-code — pinned scenes,
scrubbed timelines, a char-split hero reveal, horizontal pinned runs with
containerAnimation parallax, velocity-skew, counters, marquees — arranged to tell
_this_ brand's journey (Step 1's walkthrough is your storyboard). Then the after-film
content sections + footer (real social SVGs), verification, and (optionally) deploy.

Critical ordering law: **create ScrollTriggers for ambient/background effects AFTER
pinned scenes** — creation order is refresh order; violating this silently mis-positions
everything after a pin spacer.

---

## LANE B — Cinematic footage (any image-to-video engine)

Read `references/playbook.md` first — it is the law for this lane. The playbook and
`scripts/chain-step.sh` implement the **Higgsfield Seedance** reference path out of the
box. For any other engine (Kie.ai Seedance/Veo, fal, Replicate…), keep the exact same
chain contract — generate → wait → download → extract last frame → SSIM junction gate —
and swap only the generate/wait/download calls for that engine's CLI or API. In brief:

1. **Storyboard** the chosen concept as N chapters (5 is the sweet spot), one continuous
   camera direction the whole way down.
2. **Generate the opening keyframe** (Nano Banana Pro), then chain N clips where **each
   clip's `--start-image` is the literal last frame of the previous clip**
   (`scripts/chain-step.sh` does generate → wait → download → extract frames → SSIM
   junction gate). Draft the chain cheap first; master at full res only on approval.
3. **Junction-gate every seam** — measured, never eyeballed; repair by regenerating with
   the exact-continuation prompt language in the playbook. Dissolves over bad seams are
   forbidden.
4. **Assemble** with `scripts/assemble.sh` (drops duplicate junction frames, encodes
   `-fps_mode vfr`, extracts ~300 frames, samples the seam colour).
5. **Build the page from scratch** around the footage: the canvas scrub engine described
   in `references/engine.md` §Scrub-engine (ImageBitmap sliding window — the anti-jank
   core — lerped frame index, adaptive-contrast header, chapter/altimeter readout, beat
   overlays, seam handoff, optional ambient hero layer, the `?jump`/`__ready` dev
   contract). Write it for this brand; don't copy a previous site.

**~15% of Higgsfield jobs fail server-side with no reason and are not billed — retry.**

---

## THE DELEGATION MODEL (how tokens stay low)

You are the orchestrator and the designer. Spend frontier tokens only where taste lives.

| Work                                                                                                                                                         | Who does it                                                                             | Cost               |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------- | ------------------ |
| Concepts, art direction, palette, type, layout, motion, final copy, the build, design review                                                                 | **You (Claude)** — never delegated. Run design on the strongest Claude model available. | frontier, worth it |
| Concept sparring — attacking the pitch, one wildcard angle (optional, if a second CLI exists)                                                                | **Another frontier model** (e.g. GPT/Codex, Gemini) — strategy text only, never design  | one cheap call     |
| First drafts only: a chapter's video prompt, an after-film section's copy — **you review and rewrite every draft; nothing a sub-agent wrote ships unedited** | Claude **sub-agents**, fanned out in parallel                                           | cheap, parallel    |
| Frame extraction, SSIM gating, assembly, seam sampling, jank test, screenshots, deploy                                                                       | **Pure shell — no model** (`scripts/*`, ffmpeg, puppeteer, vercel)                      | ~free              |

Fan out independent pieces concurrently; keep the taste-bearing spine on yourself.

---

## COST DISCIPLINE (Lane B)

1. **Audio OFF** — `--generate-audio false`. Audio ON silently ~3×'s the bill.
2. **Confirm before spending.** Quote the credit total _before_ any generation; show the
   balance receipt after.
3. **Draft cheap, master once.** Validate the whole chain at the cheapest tier (480p/fast),
   then re-run only approved prompts at full resolution (1080p/std on the reference engine).
4. **Reuse the footage.** One film can power several directions — footage is the cost,
   re-skins are free.

---

## VERIFY (both lanes)

Implement the dev contract in every build: `?jump=<scrollY>` lands pre-scrolled with all
scroll state force-settled, and `window.__ready = true` fires only once the page is truly
ready. Then `scripts/verify.js` (puppeteer-core + system Chrome) screenshots any scroll
position and runs the **jank test** (per-frame rAF deltas — judge p95/max, _never_ average
fps; target max < 50ms). Screenshot every beat and every junction. Never ask the user to
eyeball what you can prove. Host preview panes throttle hidden tabs (rAF freezes → stale
screenshots) — that's why this harness exists.

---

## DEPLOY (opt-in, their Vercel)

Build a **lean** copy first — `index.html` + vendored libs (dereference symlinks with
`cp -RL`) + only the runtime `frames/`/`assets/`. Never upload build intermediates (raw
clips, keyframes — often 100MB+). Then `vercel deploy --prod --yes` from the lean dir.
Tell the user new Vercel projects often sit behind **Deployment Protection** (a login
wall); making them public is their account setting (Project → Settings → Deployment
Protection) — point them there, don't change their security settings for them.

---

## GUARDRAILS

- **This skill ships with zero personal data** — no API keys, no accounts, no personal
  paths. Every user brings their own video engine + Vercel. Never bake credentials in.
- Design + build stay on Claude. Mechanical work goes to code; design never does.
- Confirm credits before spending; show the receipt after.
- One continuous shot; one world per brand; no visible seams; no dissolve masking.
- Respect `prefers-reduced-motion` in every build.
- Reference files: `references/playbook.md` (footage law), `references/engine.md`
  (build recipes), `scripts/chain-step.sh`, `scripts/assemble.sh`, `scripts/verify.js`.
