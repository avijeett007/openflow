# OpenFlow Skills

Reusable [Claude Code skills](https://docs.anthropic.com/en/docs/claude-code) we use to build
and ship OpenFlow itself — shared so you can use them too.

| Skill | What it does | Proof it works | Credit |
|---|---|---|---|
| [`scroll-film-studio`](./scroll-film-studio/) | Builds a cinematic scroll-film website: the whole hero is one continuous shot that plays as the visitor scrolls, then melts into your content. | [openflow.computer](https://openflow.computer) — our homepage was built with it. | [Jack Roberts](https://www.youtube.com/@Itssssss_Jack) |

## Installing a skill

Skills are just folders. Copy one into a place Claude Code looks for skills:

```bash
# personal (available in every project):
cp -R skills/scroll-film-studio ~/.claude/skills/

# or project-scoped (available only inside one repo):
mkdir -p your-repo/.claude/skills
cp -R skills/scroll-film-studio your-repo/.claude/skills/
```

Then in any Claude Code session:

```
/scroll-film-studio
```

Claude runs the skill's process — it interviews you, pitches concepts, and builds the site.
No configuration files, no accounts required to start (see each skill's guide for optional
extras).

## Requirements

- [Claude Code](https://claude.com/claude-code) (CLI, desktop, or IDE extension)
- `ffmpeg` and Node ≥ 20 on your PATH
- Google Chrome (used headlessly by the verification harness)
- Optional, for AI-generated footage: a [Higgsfield](https://higgsfield.ai) account
  (`npm i -g @higgsfield/cli`) or any image-to-video engine that accepts a start image

Each skill folder contains its own guide — start with
[`scroll-film-studio/GUIDE.md`](./scroll-film-studio/GUIDE.md).

## Credits & origins

Skills in this folder come from the wider Claude Code community as well as our own work.
Original authors are credited in the table above, and we keep that credit intact as skills
evolve here.

- **`scroll-film-studio`** — originally from **Jack Roberts**
  ([youtube.com/@Itssssss_Jack](https://www.youtube.com/@Itssssss_Jack)). Go watch his
  channel — the scroll-film concept and workflow started there; our copy carries the
  refinements we made while shipping [openflow.computer](https://openflow.computer) with it.

If you're an original author of a skill here and want the attribution changed (or the
skill removed), open an issue and we'll sort it immediately.
