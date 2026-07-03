# Contributing to OpenFlow

Thanks for your interest in OpenFlow. Please read this first — the contribution
policy here is different from most open-source projects.

## ⚠️ Contributions are not being accepted right now

OpenFlow is a **solo project** maintained alongside a full-time job and a startup,
**[knotie.ai](https://knotie.ai)**. I don't have the bandwidth to review, test, and
maintain incoming pull requests, so **unsolicited PRs will generally not be
merged** and may be closed without a full review. This isn't personal — it's the
only way to keep the project sustainable for me right now.

**What helps instead:**

- 🐛 **Found a bug?** [Open an issue](https://github.com/avijeett007/openflow/issues)
  with clear reproduction steps. Bug reports are genuinely useful and I read them.
- 💡 **Want a feature?** [Open an issue](https://github.com/avijeett007/openflow/issues)
  describing the problem you're trying to solve. I keep adding features over time —
  I just can't promise a timeline.
- 🍴 **Want to change something yourself?** OpenFlow is **MIT-licensed** — fork it
  and make it your own. That's encouraged.

If you open a PR anyway, note that `main` is a **protected branch**: every PR must
pass CI (`rust-tests` + `code-quality`) and needs an approving review from a
maintainer before it can be merged. A green PR is not a guarantee it will be
merged.

## Reporting a bug

Before filing, please:

1. **Search [existing issues](https://github.com/avijeett007/openflow/issues)** in
   case it's already reported.
2. **Try the [latest release](https://github.com/avijeett007/openflow/releases/latest)**
   to see if it's already fixed.
3. **Enable debug mode** (`Cmd/Ctrl+Shift+D`) to gather diagnostics.

Include in the report (the [bug report template](.github/ISSUE_TEMPLATE/bug_report.md)
prompts for these):

- OpenFlow version, OS + version, CPU / GPU
- What you did, what you expected, and what actually happened
- Logs or screenshots if relevant

## Building & running from source (for forkers)

Prerequisites: [Rust](https://rustup.rs/) (stable) and [Bun](https://bun.sh/). The
full, platform-specific setup — including the Apple Silicon vs Intel differences
and Windows long-path handling — is in **[BUILD.md](BUILD.md)**.

```bash
git clone git@github.com:avijeett007/openflow.git
cd openflow
bun install
bun run tauri dev
# macOS, if you hit a cmake error:
CMAKE_POLICY_VERSION_MINIMUM=3.5 bun run tauri dev
```

Architecture overview and where things live: see **[AGENTS.md](AGENTS.md)** and the
README.

## Code style (if you do send a PR)

- **Rust:** `cargo fmt` + `cargo clippy` clean; explicit error handling (no
  `unwrap` in production paths); doc comments on public APIs.
- **TypeScript / React:** strict types (no `any`), functional components, Tailwind
  for styling. All user-facing strings go through i18next
  (`src/i18n/locales/en/translation.json`) — ESLint enforces this.
- **Commits:** conventional prefixes (`feat:`, `fix:`, `docs:`, `refactor:`,
  `test:`, `chore:`).
- Run `bun run format` and `bun run lint` before pushing — CI runs both.
- **AI-assisted PRs** are fine — just disclose in the PR description which tools you
  used and roughly how much.

## Credit

OpenFlow is a fork of **[Handy](https://github.com/cjpais/Handy)** by CJ Pais and
contributors (MIT). The audio pipeline, transcription managers, and Tauri
foundation come from their work — please consider supporting the upstream project
too.

## License

By contributing, you agree that your contributions are licensed under the **MIT
License** (see [LICENSE](LICENSE)).

---

<sub>Maintained by the team behind <a href="https://knotie.ai"><strong>knotie.ai</strong></a> — where you can white-label and sell AI services.</sub>
