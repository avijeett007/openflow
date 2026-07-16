//! On-device speaker diarization for OpenFlow Meetings (M2).
//!
//! Wraps the **offline** sherpa-onnx speaker-diarization pipeline (pyannote
//! segmentation-3.0 → CAM++ speaker embeddings → clustering) behind a small
//! engine type, and provides the pure, engine-independent glue the meeting
//! pipeline needs around it:
//!
//! - [`fuse_speaker_labels`] — assign each transcript segment the diarization
//!   turn it overlaps most (the whisperX-style max-time-overlap fusion,
//!   DESIGN-meetings.md §5.5). This is where turns meet the transcript.
//! - [`stabilize_labels`] — remap a fresh diarization's cluster indices onto the
//!   previous cycle's labels so provisional "Speaker N" tags don't flip between
//!   the ~30 s re-diarization passes.
//! - [`permutation_accuracy`] — best-permutation, time-weighted label accuracy
//!   against a ground truth (the ≥90 % verification metric, §10 M2).
//! - [`provisional_default_enabled`] — the Intel auto-degrade decision: given a
//!   measured diarization-to-realtime ratio, is live chunked re-diarization even
//!   feasible, or should we default to a single final pass? (§5.4, risk #2.)
//!
//! Only the mic-vs-system split makes this tractable: the mic channel is "You"
//! by construction, so diarization runs **only on the remote (system) channel**
//! (§5.4). The native engine is macOS-only, matching the CoreAudio-tap capture
//! path — a diarizable recording only ever exists on macOS. All the pure logic
//! above compiles and is unit-tested on every platform.

use serde::{Deserialize, Serialize};
use specta::Type;

/// Clustering cosine threshold for the CAM++ embeddings. Higher merges more
/// (fewer clusters). 0.7 was validated on the ground-truth harness
/// (`examples/diarize_ground_truth.rs`): with the `zh_en` advanced CAM++ export
/// it recovers the exact speaker count with ~99.7 % time-weighted accuracy and
/// is stable across 0.6–0.8. `num_clusters = -1` lets the engine estimate the
/// count (meetings don't know it up front).
pub const DIARIZATION_THRESHOLD: f32 = 0.7;

/// Sentinel meaning "estimate the number of speakers".
pub const DIARIZATION_AUTO_CLUSTERS: i32 = -1;

/// Diarization wall-time as a fraction of realtime measured on the Intel
/// i9-9980HK reference machine (`verification/meetings-m2`): ~0.14×. Used to
/// reason about live-provisional feasibility at runtime — see
/// [`provisional_default_enabled`]. A conservative reference, not a per-machine
/// measurement.
pub const REFERENCE_RATIO_RT: f32 = 0.14;

/// The between-cycle budget (seconds) a provisional re-diarization must fit
/// inside to keep live labels current.
pub const PROVISIONAL_CYCLE_BUDGET_S: f32 = 25.0;

/// One diarization turn: a `[start_ms, end_ms]` span attributed to a per-meeting
/// speaker cluster ordinal (0-based, engine-assigned). The mic channel never
/// produces these (it is "You"); only the remote channel is diarized.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Type)]
pub struct DiarTurn {
    pub start_ms: i64,
    pub end_ms: i64,
    /// Per-meeting cluster ordinal. Stored in `meeting_segments.local_speaker`.
    pub speaker: i64,
}

impl DiarTurn {
    fn overlap_ms(&self, start_ms: i64, end_ms: i64) -> i64 {
        (self.end_ms.min(end_ms) - self.start_ms.max(start_ms)).max(0)
    }

    /// Distance from a point in time to this turn (0 while inside it). Used to
    /// snap a transcript segment that overlaps no turn to the nearest one.
    fn gap_to(&self, mid_ms: i64) -> i64 {
        if mid_ms < self.start_ms {
            self.start_ms - mid_ms
        } else if mid_ms > self.end_ms {
            mid_ms - self.end_ms
        } else {
            0
        }
    }
}

/// Assign each transcript segment a speaker cluster by **maximum time overlap**
/// with the diarization turns (DESIGN-meetings.md §5.5). A segment that overlaps
/// no turn (a gap) falls back to the nearest turn by mid-point distance, so every
/// spoken segment gets a label rather than being dropped. Returns one
/// `(segment_id, Option<local_speaker>)` per input segment, in input order;
/// `None` only when there are no turns at all.
///
/// `segments` are `(id, t_start_ms, t_end_ms)` — the remote-channel segments to
/// label. Pure and deterministic: this is the unit under test.
pub fn fuse_speaker_labels(
    segments: &[(i64, i64, i64)],
    turns: &[DiarTurn],
) -> Vec<(i64, Option<i64>)> {
    segments
        .iter()
        .map(|&(id, start, end)| {
            if turns.is_empty() {
                return (id, None);
            }
            // Pick the turn with the largest overlap; ties break toward the
            // earlier turn (stable). If nothing overlaps, snap to nearest.
            let mut best_overlap = 0i64;
            let mut best_speaker: Option<i64> = None;
            for t in turns {
                let ov = t.overlap_ms(start, end);
                if ov > best_overlap {
                    best_overlap = ov;
                    best_speaker = Some(t.speaker);
                }
            }
            if best_speaker.is_none() {
                let mid = start + (end - start) / 2;
                let nearest = turns
                    .iter()
                    .min_by_key(|t| t.gap_to(mid))
                    .expect("turns is non-empty");
                best_speaker = Some(nearest.speaker);
            }
            (id, best_speaker)
        })
        .collect()
}

/// Remap the cluster ordinals of a *fresh* diarization (`next`) so they line up
/// with the `previous` cycle's ordinals, keeping provisional "Speaker N" labels
/// stable as the ~30 s chunked re-diarization re-runs over more audio
/// (DESIGN-meetings.md §5.4). Each new cluster inherits the previous ordinal it
/// overlaps most in time; new clusters with no overlap get fresh ordinals above
/// the previous maximum. Returns the remapped turns (input is not mutated).
pub fn stabilize_labels(previous: &[DiarTurn], next: &[DiarTurn]) -> Vec<DiarTurn> {
    if previous.is_empty() {
        return next.to_vec();
    }
    // For each new cluster id, accumulate temporal overlap against each previous
    // cluster id, then greedily bind the strongest pairs (one-to-one).
    let new_ids: Vec<i64> = distinct_speakers(next);
    let prev_max = previous.iter().map(|t| t.speaker).max().unwrap_or(-1);

    // overlap[(new, prev)] = total overlapping ms.
    let mut pairs: Vec<(i64, i64, i64)> = Vec::new(); // (overlap, new_id, prev_id)
    for &n in &new_ids {
        for &p in &distinct_speakers(previous) {
            let mut ov = 0i64;
            for nt in next.iter().filter(|t| t.speaker == n) {
                for pt in previous.iter().filter(|t| t.speaker == p) {
                    ov += nt.overlap_ms(pt.start_ms, pt.end_ms);
                }
            }
            if ov > 0 {
                pairs.push((ov, n, p));
            }
        }
    }
    // Greedy: strongest overlap first, each new/prev id used once.
    pairs.sort_by(|a, b| b.0.cmp(&a.0));
    let mut map: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    let mut used_prev: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for (_, n, p) in pairs {
        if map.contains_key(&n) || used_prev.contains(&p) {
            continue;
        }
        map.insert(n, p);
        used_prev.insert(p);
    }
    // Unmapped new clusters get fresh ids above the previous max, deterministically.
    let mut fresh = prev_max + 1;
    for &n in &new_ids {
        map.entry(n).or_insert_with(|| {
            let id = fresh;
            fresh += 1;
            id
        });
    }
    next.iter()
        .map(|t| DiarTurn {
            speaker: map[&t.speaker],
            ..*t
        })
        .collect()
}

fn distinct_speakers(turns: &[DiarTurn]) -> Vec<i64> {
    let mut v: Vec<i64> = turns.iter().map(|t| t.speaker).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// A labeled ground-truth span for accuracy scoring: `[start_ms, end_ms]` known
/// to belong to `label` (an arbitrary integer speaker id).
#[derive(Clone, Copy, Debug)]
pub struct GroundTruthSpan {
    pub start_ms: i64,
    pub end_ms: i64,
    pub label: i64,
}

/// Time-weighted label accuracy of `pred` turns against `truth`, under the best
/// bijective mapping of predicted clusters → true labels (DESIGN-meetings.md §10
/// M2 — "best-permutation mapping", ≥90 % bar). Samples the union of truth spans
/// every `step_ms` and counts the fraction of *voiced* truth time whose predicted
/// cluster maps to the correct true label. Returns 0.0 when there is no truth.
///
/// This is the exact metric the ground-truth harness reports, factored out so it
/// is unit-tested independently of the native engine.
pub fn permutation_accuracy(truth: &[GroundTruthSpan], pred: &[DiarTurn], step_ms: i64) -> f64 {
    if truth.is_empty() {
        return 0.0;
    }
    let step = step_ms.max(1);
    // Collect (true_label, predicted_cluster?) at each sampled instant.
    let mut samples: Vec<(i64, Option<i64>)> = Vec::new();
    for span in truth {
        let mut t = span.start_ms;
        while t < span.end_ms {
            let pred_cluster = pred
                .iter()
                .find(|turn| turn.start_ms <= t && t < turn.end_ms)
                .map(|turn| turn.speaker);
            samples.push((span.label, pred_cluster));
            t += step;
        }
    }
    if samples.is_empty() {
        return 0.0;
    }
    let true_labels = {
        let mut v: Vec<i64> = truth.iter().map(|s| s.label).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    let pred_clusters = {
        let mut v: Vec<i64> = samples.iter().filter_map(|s| s.1).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    // Best injective map true_label -> predicted_cluster maximizing agreement.
    let best = best_bijection_score(&true_labels, &pred_clusters, &samples);
    best as f64 / samples.len() as f64
}

/// Enumerate injective assignments of `true_labels` onto `pred_clusters` and
/// return the maximum number of samples correctly labeled. Speaker counts here
/// are tiny (a handful), so the factorial search is trivial.
fn best_bijection_score(
    true_labels: &[i64],
    pred_clusters: &[i64],
    samples: &[(i64, Option<i64>)],
) -> usize {
    let mut best = 0usize;
    let mut chosen: Vec<Option<i64>> = vec![None; true_labels.len()];
    let mut used = vec![false; pred_clusters.len()];
    permute(
        0,
        true_labels,
        pred_clusters,
        &mut chosen,
        &mut used,
        samples,
        &mut best,
    );
    best
}

#[allow(clippy::too_many_arguments)]
fn permute(
    i: usize,
    true_labels: &[i64],
    pred_clusters: &[i64],
    chosen: &mut Vec<Option<i64>>,
    used: &mut Vec<bool>,
    samples: &[(i64, Option<i64>)],
    best: &mut usize,
) {
    if i == true_labels.len() {
        let map: std::collections::HashMap<i64, i64> = true_labels
            .iter()
            .zip(chosen.iter())
            .filter_map(|(&t, c)| c.map(|c| (t, c)))
            .collect();
        let score = samples
            .iter()
            .filter(|(t, p)| p.is_some() && map.get(t) == p.as_ref())
            .count();
        *best = (*best).max(score);
        return;
    }
    // Option: leave this true label unmapped (when more truth than clusters).
    permute(
        i + 1,
        true_labels,
        pred_clusters,
        chosen,
        used,
        samples,
        best,
    );
    for (j, &pc) in pred_clusters.iter().enumerate() {
        if used[j] {
            continue;
        }
        used[j] = true;
        chosen[i] = Some(pc);
        permute(
            i + 1,
            true_labels,
            pred_clusters,
            chosen,
            used,
            samples,
            best,
        );
        chosen[i] = None;
        used[j] = false;
    }
}

/// The Intel auto-degrade decision (DESIGN-meetings.md §5.4, risk #2). Given the
/// measured diarization wall-time as a fraction of realtime (`ratio_rt`, e.g.
/// 0.14 means a 100 s file diarizes in 14 s), decide whether **live provisional
/// re-diarization** should be on by default.
///
/// Live mode re-diarizes the *entire accumulated* remote audio every ~30 s, so a
/// cycle costs `ratio_rt × meeting_length`. Extrapolated to the 30-minute mark, a
/// cycle over 1800 s of audio must stay within the ~25 s budget between cycles;
/// otherwise cycles pile up and labels lag hopelessly. If it can't keep up we
/// default to a single canonical pass at meeting end (the user can still opt in).
pub fn provisional_default_enabled(ratio_rt: f32) -> bool {
    provisional_cycle_budget_ok(ratio_rt, 1800.0, 25.0)
}

/// Would a full-accumulated re-diarization at `accumulated_s` of audio fit the
/// per-cycle `budget_s`? The raw feasibility test behind
/// [`provisional_default_enabled`].
pub fn provisional_cycle_budget_ok(ratio_rt: f32, accumulated_s: f32, budget_s: f32) -> bool {
    ratio_rt * accumulated_s <= budget_s
}

/* ───────────────────────────  native engine  ─────────────────────────── */

/// Which diarization mode a meeting will run, after the degrade decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum DiarizationMode {
    /// Live provisional labels (~30 s re-diarization) plus the final pass.
    Provisional,
    /// One canonical pass at meeting end only (the Intel-safe default).
    FinalOnly,
    /// Diarization disabled or models missing — segments stay "Them" (M1).
    Off,
}

#[cfg(target_os = "macos")]
mod engine {
    use super::{DiarTurn, DIARIZATION_AUTO_CLUSTERS};
    use anyhow::{anyhow, Result};
    use std::path::Path;

    /// A loaded offline diarization pipeline (segmentation + embedding +
    /// clustering). Cheap to hold; ~0.5 s to construct, ~0.14× realtime to run on
    /// the Intel i9-9980HK reference machine.
    pub struct DiarizationEngine {
        inner: sherpa_onnx::OfflineSpeakerDiarization,
        sample_rate: i32,
    }

    impl DiarizationEngine {
        /// Load the pipeline from the segmentation + embedding model files.
        /// `num_clusters < 0` estimates the speaker count.
        pub fn new(
            segmentation: &Path,
            embedding: &Path,
            threshold: f32,
            num_clusters: i32,
        ) -> Result<Self> {
            let mut cfg = sherpa_onnx::OfflineSpeakerDiarizationConfig::default();
            cfg.segmentation.pyannote.model = Some(path_str(segmentation)?);
            cfg.embedding.model = Some(path_str(embedding)?);
            cfg.clustering.num_clusters = num_clusters;
            cfg.clustering.threshold = threshold;
            let inner = sherpa_onnx::OfflineSpeakerDiarization::create(&cfg)
                .ok_or_else(|| anyhow!("failed to create sherpa-onnx diarizer (bad models?)"))?;
            let sample_rate = inner.sample_rate();
            Ok(Self { inner, sample_rate })
        }

        /// Sample rate the segmentation model expects (16 kHz for pyannote-3.0).
        pub fn sample_rate(&self) -> i32 {
            self.sample_rate
        }

        /// Diarize a complete 16 kHz mono waveform → ordered `[start,end,cluster]`
        /// turns. Returns an empty vec for silence / no detected speech.
        pub fn diarize(&self, samples: &[f32]) -> Result<Vec<DiarTurn>> {
            let result = self
                .inner
                .process(samples)
                .ok_or_else(|| anyhow!("diarization process returned null"))?;
            Ok(result
                .sort_by_start_time()
                .into_iter()
                .map(|s| DiarTurn {
                    start_ms: (s.start * 1000.0).round() as i64,
                    end_ms: (s.end * 1000.0).round() as i64,
                    speaker: s.speaker as i64,
                })
                .collect())
        }
    }

    /// Convenience constructor with the validated default threshold + auto count.
    pub fn open_default(segmentation: &Path, embedding: &Path) -> Result<DiarizationEngine> {
        DiarizationEngine::new(
            segmentation,
            embedding,
            super::DIARIZATION_THRESHOLD,
            DIARIZATION_AUTO_CLUSTERS,
        )
    }

    fn path_str(p: &Path) -> Result<String> {
        p.to_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("non-UTF-8 model path: {}", p.display()))
    }
}

#[cfg(target_os = "macos")]
pub use engine::{open_default, DiarizationEngine};

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(start_ms: i64, end_ms: i64, speaker: i64) -> DiarTurn {
        DiarTurn {
            start_ms,
            end_ms,
            speaker,
        }
    }

    #[test]
    fn fuse_assigns_max_overlap_speaker() {
        let turns = vec![turn(0, 5000, 0), turn(5000, 10000, 1)];
        // A segment mostly inside turn 1.
        let segs = vec![(10, 4000, 9000)]; // 1000ms in spk0, 4000ms in spk1
        let out = fuse_speaker_labels(&segs, &turns);
        assert_eq!(out, vec![(10, Some(1))]);
    }

    #[test]
    fn fuse_snaps_gap_segment_to_nearest_turn() {
        let turns = vec![turn(0, 1000, 0), turn(8000, 9000, 1)];
        // Segment sits in the silent gap, closer to turn 1.
        let segs = vec![(7, 6000, 7000)];
        let out = fuse_speaker_labels(&segs, &turns);
        assert_eq!(out, vec![(7, Some(1))]);
    }

    #[test]
    fn fuse_returns_none_without_turns() {
        let out = fuse_speaker_labels(&[(1, 0, 1000)], &[]);
        assert_eq!(out, vec![(1, None)]);
    }

    #[test]
    fn fuse_ties_break_to_earlier_turn() {
        // Equal overlap with spk0 and spk1 → earlier (spk0) wins (stable).
        let turns = vec![turn(0, 1000, 0), turn(1000, 2000, 1)];
        let segs = vec![(3, 500, 1500)]; // 500ms each side
        let out = fuse_speaker_labels(&segs, &turns);
        assert_eq!(out, vec![(3, Some(0))]);
    }

    #[test]
    fn stabilize_keeps_previous_ordinals() {
        // Previous: spk0 early, spk1 late. Next run flips the raw indices.
        let previous = vec![turn(0, 5000, 0), turn(5000, 10000, 1)];
        let next = vec![turn(0, 5000, 1), turn(5000, 10000, 0)];
        let out = stabilize_labels(&previous, &next);
        // The early cluster should still read as 0, the late one as 1.
        assert_eq!(out[0].speaker, 0);
        assert_eq!(out[1].speaker, 1);
    }

    #[test]
    fn stabilize_gives_new_cluster_a_fresh_id() {
        let previous = vec![turn(0, 5000, 0)];
        // Next keeps the old speaker and introduces a brand-new one.
        let next = vec![turn(0, 5000, 0), turn(6000, 9000, 1)];
        let out = stabilize_labels(&previous, &next);
        assert_eq!(out[0].speaker, 0);
        assert_eq!(out[1].speaker, 1, "unseen cluster gets prev_max+1");
    }

    #[test]
    fn stabilize_passthrough_when_no_history() {
        let next = vec![turn(0, 5000, 3), turn(5000, 9000, 7)];
        assert_eq!(stabilize_labels(&[], &next), next);
    }

    #[test]
    fn permutation_accuracy_perfect_with_relabeled_clusters() {
        // Truth: A=0..1000, B=1000..2000. Prediction uses different ids (9,4)
        // but perfectly separates them → 100% under best permutation.
        let truth = vec![
            GroundTruthSpan {
                start_ms: 0,
                end_ms: 1000,
                label: 0,
            },
            GroundTruthSpan {
                start_ms: 1000,
                end_ms: 2000,
                label: 1,
            },
        ];
        let pred = vec![turn(0, 1000, 9), turn(1000, 2000, 4)];
        let acc = permutation_accuracy(&truth, &pred, 10);
        assert!((acc - 1.0).abs() < 1e-9, "acc={acc}");
    }

    #[test]
    fn permutation_accuracy_penalizes_merged_speakers() {
        // Both truth speakers predicted as one cluster → at most 50%.
        let truth = vec![
            GroundTruthSpan {
                start_ms: 0,
                end_ms: 1000,
                label: 0,
            },
            GroundTruthSpan {
                start_ms: 1000,
                end_ms: 2000,
                label: 1,
            },
        ];
        let pred = vec![turn(0, 2000, 0)];
        let acc = permutation_accuracy(&truth, &pred, 10);
        assert!((acc - 0.5).abs() < 1e-9, "acc={acc}");
    }

    #[test]
    fn degrade_defaults_to_final_only_on_intel_reference() {
        // Measured on the i9-9980HK: ~0.14x realtime. At the 30-min mark a full
        // re-diarization would take ~0.14*1800 = 252s >> 25s → provisional OFF.
        assert!(!provisional_default_enabled(0.14));
        // A hypothetical 5x-faster machine (0.008x) *could* keep up.
        assert!(provisional_default_enabled(0.008));
    }

    #[test]
    fn degrade_cycle_budget_boundary() {
        // Exactly at budget is OK; just over is not.
        assert!(provisional_cycle_budget_ok(0.01, 1800.0, 18.0)); // 18.0 == budget
        assert!(!provisional_cycle_budget_ok(0.0101, 1800.0, 18.0)); // 18.18 > 18
    }
}
