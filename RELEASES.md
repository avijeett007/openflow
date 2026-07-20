# Releasing OpenFlow

The single most important fact about this repo's pipeline:

> **Merging to `main` builds nothing and ships nothing. Only pushing a `v*` tag
> publishes an OTA release.** (`build.yml` publishes only on `refs/tags/v*`; on a
> branch/dispatch it uploads installers as run artifacts.)

So merges are free. Cost and user impact live entirely at the tag.

## Two separate things

|                      | Trigger                                                                           | Cost                                       | Reaches users?                                         |
| -------------------- | --------------------------------------------------------------------------------- | ------------------------------------------ | ------------------------------------------------------ |
| **Validation build** | `build.yml` on a branch (the steward dispatches it) or manual `workflow_dispatch` | CI minutes only (free on this public repo) | No — run artifacts only                                |
| **Release**          | push a `v*` tag                                                                   | CI + a published GitHub Release            | **Yes — OTA to every user via the daily update check** |

## Release policy

- **Routine dependency updates accumulate on `main`, unreleased.** They change
  dependencies, not the app version, so `main` stays at its current version and
  nothing churns. The nightly steward batches them into one integration PR
  (see [`agents/README.md`](agents/README.md)); you do the final merge.
- **Cut a `v*` tag (the only thing that ships) ONLY for real value:** a security
  fix, a user-facing feature, or a meaningful bugfix. Batch the accumulated
  dependency updates into that release's notes.
- **Agents never tag or release.** Not the crew, not the steward — the release tool
  is withheld from them. A release is always a human decision.

## How to cut a release

1. Make sure `main` is where you want it (the meaningful change is merged; routine
   deps are already batched in).
2. Bump the version in the four lockstep files: `package.json`,
   `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock`.
3. Merge that bump, then push the tag:
   ```bash
   git tag v<x.y.z> && git push origin v<x.y.z>
   ```
4. `build.yml` builds all three platforms into a **draft** release and, once all
   pass, flips it to **published** — the OTA goes live. If any platform fails, the
   release stays a draft (nothing half-ships).
5. Verify: `latest.json` advertises the new version; footer → "Check for updates"
   offers it.

## Rolling back

Nothing is auto-published except on a tag, so a bad merge on `main` never reached
users. If a _released_ version is bad, cut a follow-up patch tag — do not delete a
published release users may already be on.
