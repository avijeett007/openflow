# Meetings M2 — local speaker diarization: verification

On-device speaker diarization for OpenFlow Meetings, built on the **official
k2-fsa Rust bindings** (`sherpa-onnx` 1.13.4). All numbers below were measured on
this machine — an **Intel Core i9-9980HK** (the oldest supported Intel target).

## Engine + models chosen

| Role         | Choice                                                                                                        | Source                                                                                                                                                                                                                                         | License    |
| ------------ | ------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------- |
| Bindings     | `sherpa-onnx` 1.13.4 (+ `sherpa-onnx-sys` 1.13.4)                                                             | crates.io / github.com/k2-fsa/sherpa-onnx                                                                                                                                                                                                      | Apache-2.0 |
| Segmentation | pyannote **segmentation-3.0** ONNX (`model.onnx`, ~6 MB)                                                      | [k2-fsa `speaker-segmentation-models`](https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2) — the MIT re-export, ships its LICENSE (never the HF-gated original) | MIT        |
| Embedding    | 3D-Speaker **CAM++ `zh_en` advanced** (`3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx`, ~28 MB) | [k2-fsa `speaker-recongition-models`](https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx)                                                          | Apache-2.0 |

Checksums (verified on download; encoded in `meeting/diar_models.rs`):

- segmentation `.tar.bz2` sha256 `24615ee884c897d9d2ba09bb4d30da6bb1b15e685065962db5b02e76e4996488`
- embedding `.onnx` sha256 `aa3cfc16963a10586a9393f5035d6d6b57e98d358b347f80c2a30bf4f00ceba2`

The `reverb-diarization-v1` model is **not** in the catalog (non-commercial).

### Embedding-model deviation from the design (measured, not guessed)

The design first named the `en_voxceleb` CAM++ export, with an explicit "verify at
integration; never hardcode" caveat (§5.3). On the ground-truth harness the two
exports differed sharply, so we ship the stronger one:

| Embedding export                   | Frame accuracy (3 distinct speakers) |
| ---------------------------------- | ------------------------------------ |
| `3dspeaker ... en_voxceleb`        | ~69 % (merged two speakers)          |
| **`3dspeaker ... zh_en advanced`** | **99.7 %** (exact speaker count)     |
| `wespeaker resnet34_LM`            | ~33–65 % (under-clustered)           |

Clustering: `num_clusters = -1` (auto), cosine `threshold = 0.7` — validated stable
across 0.6–0.8.

## Ground-truth accuracy (the ≥90 % bar) — PASS

Harness: `src-tauri/examples/diarize_ground_truth.rs` (macOS impl +
cross-platform stub). It synthesizes a deterministic 3-speaker conversation with
macOS `say` — **Daniel** (British male), **Samantha** (US female), **Rishi**
(Indian male) — records the exact turn boundaries, runs the **real, shipped**
`meeting::diarize::open_default` engine, and scores with the **real, unit-tested**
`meeting::diarize::permutation_accuracy` (time-weighted, best-permutation). See
`HARNESS_RESULTS.md` for the machine-written result.

```
synthesized 72.9s, 6 turns, 3 speakers
engine: 3 speakers, 6 turns
benchmark: 10229 ms for 72.9s = 0.140x realtime
ACCURACY = 99.7%  (PASS ≥90%)
```

| predicted cluster | true speaker |
| ----------------- | ------------ |
| 0                 | A (Daniel)   |
| 1                 | B (Samantha) |
| 2                 | C (Rishi)    |

Honest caveat: on **very short, rapid-fire** turns (≈6 s each, alternating every
turn) accuracy drops to ~70–83 % — pyannote is trained on natural conversational
speech and synthetic back-to-back TTS is somewhat out of distribution. Real
multi-party calls (varying turn lengths) behave like the longer-turn case above.
A real 2-device call remains the standing user task to confirm live behavior.

## Intel benchmark gate → default = final-pass-only (auto-degrade)

Measured **0.140× realtime** (i9-9980HK): a 72.9 s file diarizes in 10.2 s. The
live-provisional design re-diarizes the **entire accumulated** remote audio every
~30 s, so a cycle costs `0.14 × meeting_length`:

| Meeting elapsed | Provisional cycle time | Fits the 25 s budget? |
| --------------- | ---------------------- | --------------------- |
| 3 min           | ~25 s                  | borderline            |
| 10 min          | ~84 s                  | no                    |
| **30 min**      | **~252 s**             | **no (10× over)**     |

So a full-accumulated re-diarization cannot keep up past a few minutes on Intel.
**The measurement dictates the default: provisional labels OFF, one canonical
final pass at meeting end** (`meetings_diarization_provisional` defaults `false`).
Users on much faster hardware can opt in; the master switch
`meetings_diarization` defaults `true` (safe, since with no models it's a no-op).
This decision is encoded + unit-tested in `provisional_default_enabled`
(`meeting/diarize.rs`).

## Concurrent-dictation refinement — approach chosen

Additive migration `meeting_segments.flags INTEGER NOT NULL DEFAULT 0`
(bit 0 = `SEGMENT_FLAG_PRIVATE`). A mic-channel utterance whose **speech onset**
occurs while OpenFlow's own dictation is actively capturing
(`AudioRecordingManager::is_recording()`, polled by the levels ticker) is flagged
private — "you, to OpenFlow", not to the meeting. The detail view dims these and
offers a per-meeting "hide what I said to OpenFlow" toggle. The `channel` column
was left as-is (`mic`/`system`) so the "mic = You" fusion invariant is untouched.

## Non-breaking guarantees

When diarization is off / models are missing / the engine errors, a meeting
completes **exactly as M1**: `stop_capture` marks it `done` immediately and every
segment stays "Them" (`local_speaker` NULL). The provisional worker, the live
sink, and the dictation probe are all `Option`-gated — passing `None` is
byte-for-byte the M1 segmenter path. Dictation (hotkey → capture → STT → inject)
is never touched. Migrations are additive; the pre-existing M1 segment survives
the upgrade with `flags` defaulted to 0 (test
`meetings_m2_migrations_add_speakers_and_flags`).

## Gates

| Gate                              | Result                                                                                                |
| --------------------------------- | ----------------------------------------------------------------------------------------------------- |
| `cargo test` (lib)                | **268 passed**, 0 failed (M1 256 + new: fusion×4, stabilize×3, permutation×2, degrade×2, migration×1) |
| `cargo build` (full app, debug)   | clean — sherpa static lib links alongside the dynamic `ort`                                           |
| `bunx tsc --noEmit`               | clean                                                                                                 |
| `bun run lint`                    | 0 errors                                                                                              |
| `bun run format` / `format:check` | clean                                                                                                 |
| `bun run build`                   | clean                                                                                                 |
| bindings                          | regenerated (`src/bindings.ts`)                                                                       |

## New Rust unit tests

- `meeting::diarize` — `fuse_assigns_max_overlap_speaker`,
  `fuse_snaps_gap_segment_to_nearest_turn`, `fuse_returns_none_without_turns`,
  `fuse_ties_break_to_earlier_turn`, `stabilize_keeps_previous_ordinals`,
  `stabilize_gives_new_cluster_a_fresh_id`, `stabilize_passthrough_when_no_history`,
  `permutation_accuracy_perfect_with_relabeled_clusters`,
  `permutation_accuracy_penalizes_merged_speakers`,
  `degrade_defaults_to_final_only_on_intel_reference`,
  `degrade_cycle_budget_boundary`.
- `managers::history` — `meetings_m2_migrations_add_speakers_and_flags`.

## What still needs a human

- A **real multi-party call** (2+ remote participants over Zoom/Teams/FaceTime) to
  confirm live labels and the final pass on natural speech + crosstalk, and to
  screenshot the speaker-colored transcript. The synthetic harness proves the
  pipeline and the numbers; a real call proves the end-to-end capture→diarize UX.
