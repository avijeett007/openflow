# The Scroll-Film Playbook (Lane B — cinematic footage)

Hard-won rules for making the whole page one continuous Higgsfield film. These are a
floor, not a ceiling — break them knowingly, never by accident.

## 1. Footage-first law
The film is the source of truth; the website is a player. Design the camera arc first
(one continuous journey, ~5 chapters), then build the page around whatever footage
actually comes back. Never storyboard the site and force footage to match — footage
drifts, copy is cheap to move.

## 2. Chaining law (flawless joins)
Each clip's `--start-image` is the **ffmpeg-extracted literal last frame** of the previous
clip — not a lookalike keyframe, the actual pixels:

```bash
ffmpeg -sseof -0.05 -i clipN.mp4 -update 1 -q:v 1 clipN-last.png
higgsfield generate create seedance_2_0 --prompt "..." \
  --start-image clipN-last.png --duration 5 --resolution 1080p \
  --mode std --generate-audio false
```

Only the opening keyframe (Nano Banana Pro) starts the chain; every later start-image is a
real last frame. Keep one continuous camera direction (always descending / always pushing
in) — reversals read as cuts. Uniform clip length = constant scrub speed.

## 3. The junction gate (measured, never eyeballed)
```bash
ffmpeg -i A-last.png -i B-first.png -lavfi ssim -f null - 2>&1 | grep All
```
- **≥ 0.88 pass** · 0.80–0.88 watch it in motion · a true fail is **structural**.
- SSIM under-reads on stochastic texture (clouds ~0.66, embers ~0.72, liquid caustics
  ~0.60 can all be seamless). The number says *where* to look; the side-by-side decides.
- The #1 real failure is **grade/geometry drift** (an invented sunrise, a new horizon).
  Fix by regenerating with: *"Continue the exact same shot from the reference frame,
  identical framing, identical colour grade. Do not change the colour grade."*
- **Dissolves/crossfades over a bad junction are forbidden** — the scrub lets the user
  park on the seam, which exposes the mask instantly. Fix the join, don't hide it.

## 4. Billing truths (verify by balance delta, not docs)
- `--generate-audio false` is *the* cost lever — audio ON silently ~3×'s the bill.
- Measured price ladder per 5s clip (confirm with `higgsfield generate cost`):
  1080p/std ≈ 45 · 720p/std ≈ 22.5 · 720p/fast ≈ 17.5 · 480p/fast ≈ 7.5. 10s = 2×5s.
- **Draft the whole chain at 480p/fast to validate, then re-run approved prompts at
  1080p.** A regen at draft tier costs a fraction of a full one.
- ~15% of jobs fail server-side with no reason and don't bill — just retry the same call.

## 5. Assembly
- Concat dropping the duplicate junction frame (`select='gte(n,1)'` on clips 2+), and
  **always `-fps_mode vfr`** on the master encode — default CFR sync pads ~5 dup frames per
  junction = frozen scrub zones.
- Extract every 2nd frame to ~300 JPEGs at ~1280px, `-q:v 4`. (Dark, grainy footage nearly
  doubles JPEG bytes — 1280/q4 keeps the payload light without visible loss at cover-fit.)
- Sample the final frame's edge colour → the seam hex for the film→content handoff.

`scripts/chain-step.sh` and `scripts/assemble.sh` do all of this.

## 6. The scrub engine (why it's jank-free)
- **Canvas + pre-extracted JPEGs**, never `<video currentTime>` scrubbing (seek stutter).
- **ImageBitmap sliding window**: `drawImage(HTMLImageElement)` forces a *synchronous* JPEG
  decode on first paint (and after cache eviction) — that decode spike *is* the frame-by-
  frame jank. `createImageBitmap` decodes off-thread; keep a window of decoded bitmaps
  around the playhead (±18 ahead, evict/close beyond ±28) so every draw is a pure GPU blit.
- Lerp the frame index (`current += (target-current)*0.14`) for butter. Cap DPR at ~1.5.
- Lenis smooth scroll; a concurrency-capped image pump; `nearestFrame()` fallback so a
  missing frame never blanks the canvas.
- **Measure jank with rAF deltas (p95/max), not average fps.** Target max < 50ms.

## 7. Chrome, seam, and the ambient layer
- **Adaptive header**: sample the drawn frame's top strip luminance (~every 180ms) → toggle
  a `.on-light` class. Fixed chrome over changing film can't be one hard-coded colour.
- **Seamless handoff**: start the next section's background gradient at the *sampled* final-
  frame colour. No visible line between film and content.
- **Ambient hero layer** (optional, free): sprite-based canvas particles themed to the world
  (snow glisten, gold pollen) over the static first frame, fading out across the first ~7%
  of scroll — the hero feels alive before the scrub starts. Use one offscreen radial-gradient
  sprite + `drawImage` per particle (never `shadowBlur`); stop rendering entirely at alpha 0.
- Film grain + vignette sell the "one shot" feel; fade both out with the handoff.

## 8. Verification harness
Host preview panes throttle hidden tabs (rAF freezes → stale screenshots). The reliable path:
puppeteer-core + system Chrome + a page dev-contract:
- `?jump=<scrollY>` → land pre-scrolled and force-settle all scroll state.
- `window.__ready = true` only after frames are decoded and settled.
- Capture: `goto → waitForFunction(__ready) → wait ~1200ms → screenshot`. Shoot every beat
  position *and* every junction. Hide any cursor-follower until first real mousemove or it
  photobombs captures at 0,0.

`scripts/verify.js` does capture + jank-test.

## 9. Governance
Design taste and design code are done by the Claude model only. Mechanical steps (ffmpeg,
SSIM, puppeteer, vercel) are pure code — no model. Quote credits before spending; show the
receipt after. One continuous shot, one world per brand.
