# Engine recipes — how to build the page (both lanes)

Not a template. These are the load-bearing mechanics you write _into_ each bespoke
build. Everything else — markup, styling, motion shape, copy — you design fresh per
brand.

---

## Scrub engine (Lane B) — canvas + frames, never `<video>`

`<video currentTime>` scrubbing stutters (seek latency). The only jank-free path:
pre-extracted JPEG frames drawn to a full-viewport `<canvas>`, driven by scroll.

**Structure:** a tall scroll driver (`~170vh per chapter`, e.g. `850vh` for 5) containing
a `position:sticky; top:0; height:100vh` stage with the canvas + overlays. Film progress:

```js
const r = filmScroll.getBoundingClientRect();
const p = Math.max(0, Math.min(1, -r.top / (r.height - innerHeight)));
```

**Lerped playhead** (this is the butter — direct mapping feels mechanical):

```js
currentFrame += (target - currentFrame) * 0.14; // target = p * (FRAME_COUNT - 1)
```

**The anti-jank core — ImageBitmap sliding window.** `drawImage(HTMLImageElement)`
forces a _synchronous_ JPEG decode on the main thread at first paint and again after
browser cache eviction — those decode spikes are the "frame-by-frame glitchy" feel.
Decode off-thread around the playhead so every draw is a pure GPU blit:

```js
const bitmaps = new Map(),
  decoding = new Set();
const B_AHEAD = 18,
  B_KEEP = 28;
let bmpCenter = -999;
function ensureBitmaps(center) {
  if (Math.abs(center - bmpCenter) < 3) return;
  bmpCenter = center;
  const lo = Math.max(0, center - B_AHEAD),
    hi = Math.min(FRAME_COUNT - 1, center + B_AHEAD);
  for (let i = lo; i <= hi; i++) {
    if (bitmaps.has(i) || decoding.has(i) || !images[i]) continue;
    decoding.add(i);
    createImageBitmap(images[i])
      .then((b) => {
        decoding.delete(i);
        if (Math.abs(i - bmpCenter) > B_KEEP) {
          b.close();
          return;
        }
        bitmaps.set(i, b);
        if (i === displayed) drawFrame(i, true); // repaint if the shown frame upgraded
      })
      .catch(() => decoding.delete(i));
  }
  for (const k of Array.from(bitmaps.keys()))
    if (k < center - B_KEEP || k > center + B_KEEP) {
      bitmaps.get(k).close();
      bitmaps.delete(k);
    }
}
// draw: prefer bitmaps.get(idx), fall back to nearest loaded HTMLImageElement
```

Call `ensureBitmaps(Math.round(currentFrame))` every tick, **pre-warm around frame 0 at
boot**, and cap `devicePixelRatio` at **1.5** (2.0 doubles blit cost for invisible gain).

**Frame loading:** concurrency-capped pump (~10 in flight) into an array, a loader with a
real progress bar, and a `nearestFrame()` fallback (scan outward from the requested index)
so a missing frame never blanks the canvas.

**Frame payload:** ~300 frames, ~1280px wide, JPEG `-q:v 4`. Dark/grainy footage nearly
doubles JPEG bytes — resist going bigger.

## Beat overlays (copy over the film)

Absolute-positioned overlays with progress envelopes, driven from the same tick:

```html
<div class="beat" data-in="0.16" data-peak="0.235" data-out="0.31">
  <h2>…</h2>
</div>
```

```js
function beatAlpha(b, p) {
  if (p < b.in || p > b.out) return 0;
  if (p < b.peak) return (p - b.in) / Math.max(1e-4, b.peak - b.in);
  if (b.out > 1.5) return 1; // finale: data-out="2" never fades
  return 1 - (p - b.peak) / Math.max(1e-4, b.out - b.peak);
}
// alpha → style.opacity, plus a small translateY against scroll direction
```

Hero beat must be visible at scroll 0: `data-in="-0.1" data-peak="0"`. If the finale
frame is a centred product/subject, anchor the finale panel to the left so the subject
stays hero.

## Adaptive header (fixed chrome over a changing film)

Sample the drawn frame's top strip every ~180ms into a 16×4 offscreen canvas, average
luminance, toggle an `.on-light` class (threshold ≈ 138). All header colours run through
`currentColor` so one class flips everything. A **chapter readout** (label + thin progress
bar) doubles as narrative and progress UI — or theme it (e.g. a live altimeter counting
down as the film descends).

## Seam handoff (film → content, no visible line)

The assembly script samples the film's final-frame bottom-strip colour. Start the next
section's background **at exactly that hex**, and add a bottom-fade overlay on the film
stage that ramps in over the last ~8% of progress (`(p - 0.92) / 0.08`). Fade the grain +
vignette out with the same ramp. If the film ends dark and the content is light, build a
tall gradient "landing zone" that melts dark → brand-light over the first content block.

## Ambient hero layer (optional, free, sells the opening)

Themed canvas particles (snow glisten, gold pollen, embers) over the static first frame,
fading out across the first ~7% of scroll: one 32px offscreen radial-gradient sprite,
`drawImage` per particle with per-particle depth (size/speed/alpha), sin-based twinkle or
glow pulse. Never `shadowBlur` (expensive). Stop rendering entirely once alpha hits 0.
Skip under `prefers-reduced-motion`.

## The dev contract (verification hooks — implement in every build)

```js
const JUMP = new URLSearchParams(location.search).get("jump");
if (JUMP !== null) history.scrollRestoration = "manual"; // and skip smooth-scroll init
// after everything is loaded and settled:
if (JUMP !== null) {
  scrollTo(0, +JUMP || 0); /* recompute progress, draw, tick once */
}
window.__ready = true;
```

`?jump=<y>` must land pre-scrolled with all scroll-driven state force-settled (for
pure-code builds: `ScrollTrigger.update()` then set each scrubbed animation's
`totalProgress` explicitly). `__ready` gates the screenshot harness. Hide any
cursor-follower until the first real `mousemove` or it photobombs captures at (0,0).

Jank meter for the console: track per-frame rAF deltas, log `max` every 2s. Judge p95/max,
never average fps — a 60fps average hides 80ms decode spikes perfectly.

---

## Pure-code film (Lane A) — the motion vocabulary

The "film" is a sequence of scroll-driven scenes. Wire Lenis into GSAP's ticker:

```js
const lenis = new Lenis({ lerp: 0.09, smoothWheel: true });
lenis.on("scroll", ScrollTrigger.update);
gsap.ticker.add((t) => lenis.raf(t * 1000));
gsap.ticker.lagSmoothing(0);
```

Vocabulary to compose from (pick what tells _this_ brand's journey):

- **Char-split hero reveal** — split the wordmark into spans, stagger `yPercent:120 → 0`
  with `power4.out`.
- **Pinned scrubbed scenes** — `pin: true, scrub: true, end: '+=140%'` timelines
  (a growing/rotating form, a blend "vortex", a mask opening to full-bleed).
- **Horizontal pinned run** — translate a `width:max-content` track by
  `-(scrollWidth - innerWidth)`; give child elements their own parallax via
  `containerAnimation`. Use `invalidateOnRefresh: true`.
- **Clip-path reveals** — `inset(0 0 100% 0) → inset(0)` on scroll for editorial rows.
- **Velocity-skew** — skew a ticker/marquee by `ScrollTrigger.getVelocity()` clamped.
- **Counters** — `once: true` triggers with `snap: { textContent: 1 }`.
- **Marquee drift** — `xPercent: -50, repeat: -1` on a doubled row.

**Ordering law (silent killer):** ScrollTriggers are refreshed in _creation order_.
Create all pinned scenes **first**, ambient/background triggers **after** — otherwise
positions computed before pin spacers exist are silently wrong (effects fire thousands
of pixels early).

Performance: GPU-only properties (transform/opacity), `will-change` on the few moving
nodes, no layout-thrashing reads in tickers. Same dev contract + jank meter as Lane B.
