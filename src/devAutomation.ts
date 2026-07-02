// Dev-only automation client for the vite automation bridge (see vite.config.ts).
// Polls for JS jobs from the external test harness, evals them in the webview,
// and posts results back. Loaded only when import.meta.env.DEV (see main.tsx).

async function poll() {
  try {
    const r = await fetch("/__auto/pull");
    const job = await r.json();
    if (job) {
      let result: string;
      try {
        // eslint-disable-next-line no-eval
        const value = await eval(`(async () => { ${job.code} })()`);
        result = JSON.stringify({ ok: true, value });
      } catch (e) {
        result = JSON.stringify({ ok: false, error: String(e) });
      }
      await fetch("/__auto/result", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ id: job.id, result }),
      });
    }
  } catch {
    // dev server briefly unavailable; keep polling
  }
  setTimeout(poll, 250);
}

poll();
export {};
