#!/usr/bin/env node
/*
 * verify.js — the visual-verification harness (mechanical lane, no model).
 *
 *   node verify.js shot   <url> <outfile.png> [width] [height]   # screenshot at ?jump position
 *   node verify.js jank   <url>                                  # scroll-through jank test
 *
 * Uses puppeteer-core + your system Chrome (host preview panes throttle hidden tabs, freezing
 * rAF and returning stale screenshots — this path is immune). The page under test must
 * implement the dev contract described in references/engine.md: ?jump=<scrollY> lands
 * pre-scrolled+settled, and window.__ready === true fires once the page is truly ready.
 * If __ready never fires, this harness FAILS — a screenshot of an unready page is not proof.
 *
 * Setup once:  npm i puppeteer-core   (and have Google Chrome installed)
 * Chrome path is auto-detected for macOS/Linux/Windows; override with CHROME_PATH=/path.
 */
const puppeteer = require('puppeteer-core');

function chromePath() {
  if (process.env.CHROME_PATH) return process.env.CHROME_PATH;
  const p = process.platform;
  if (p === 'darwin') return '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
  if (p === 'win32') return 'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe';
  return '/usr/bin/google-chrome';
}

async function withBrowser(fn) {
  const b = await puppeteer.launch({
    executablePath: chromePath(),
    headless: 'new',
    args: ['--hide-scrollbars', '--no-sandbox'],
  });
  try { return await fn(b); }
  finally { await b.close().catch(() => {}); }
}

async function ready(page) {
  await page.waitForFunction('window.__ready === true', { timeout: 45000 })
    .catch(() => { throw new Error('window.__ready never fired — page not ready, refusing to capture (implement the dev contract)'); });
}

async function shot(url, out, w = 1440, h = 900) {
  await withBrowser(async b => {
    const page = await b.newPage();
    await page.setViewport({ width: +w, height: +h, deviceScaleFactor: 1 });
    await page.goto(url, { waitUntil: 'networkidle0', timeout: 60000 });
    await ready(page);
    await new Promise(r => setTimeout(r, 1200)); // let lerps/entrances settle
    await page.screenshot({ path: out });
    console.log('captured', out);
  });
}

async function jank(url) {
  await withBrowser(async b => {
    const page = await b.newPage();
    await page.setViewport({ width: 1440, height: 900, deviceScaleFactor: 1 });
    await page.goto(url, { waitUntil: 'networkidle0', timeout: 60000 });
    await ready(page);
    const stats = await page.evaluate(() => new Promise(res => {
      const end = Math.max(0, (document.scrollingElement || document.documentElement).scrollHeight - innerHeight);
      const deltas = []; let last = performance.now(), y = 0;
      const tick = () => {
        const now = performance.now(); deltas.push(now - last); last = now;
        y += 13; window.scrollTo(0, Math.min(y, end));
        if (y < end) requestAnimationFrame(tick);
        else {
          deltas.sort((a, b) => a - b);
          const p = q => deltas[Math.floor(deltas.length * q)];
          res({
            frames: deltas.length, scrolled: end,
            avg: +(deltas.reduce((a, b) => a + b, 0) / deltas.length).toFixed(1),
            p95: +p(0.95).toFixed(1), max: +deltas[deltas.length - 1].toFixed(1),
            over50: deltas.filter(d => d > 50).length,
          });
        }
      };
      requestAnimationFrame(tick);
    }));
    console.log(JSON.stringify(stats));
    console.log(stats.max < 50 ? 'PASS (max < 50ms)' : 'JANK — investigate the bitmap window / DPR / frame weight');
    if (stats.max >= 50) process.exitCode = 2;
  });
}

const [mode, url, out, w, h] = process.argv.slice(2);
(async () => {
  if (mode === 'shot') await shot(url, out, w, h);
  else if (mode === 'jank') await jank(url);
  else { console.error('usage: node verify.js shot <url> <out.png> [w] [h]  |  node verify.js jank <url>'); process.exit(1); }
})().catch(e => { console.error(e.message); process.exit(1); });
