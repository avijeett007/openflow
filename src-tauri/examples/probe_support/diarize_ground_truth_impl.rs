// Ground-truth verification harness for OpenFlow Meetings M2 diarization
// (DESIGN-meetings.md §10 M2: "scripted 3-speaker test file → final-pass labels
// match ground truth on ≥90 % of segments").
//
// Fully standalone — it needs no running app. It:
//   1. synthesizes a deterministic 3-speaker conversation with macOS `say`
//      (Daniel = British male, Samantha = US female, Rishi = Indian male — three
//      clearly distinct timbres, which real meetings also have), recording the
//      exact speaker boundaries;
//   2. runs the SAME offline pipeline the app uses, through the *real*
//      `meeting::diarize::open_default` (pyannote segmentation-3.0 + 3D-Speaker
//      CAM++ zh_en advanced embeddings, threshold 0.7, auto speaker count);
//   3. scores with the *real*, unit-tested `meeting::diarize::permutation_accuracy`
//      (time-weighted best-permutation), and prints PASS/FAIL against 90 %.
//
// Because it drives the shipped library code, "it passed the harness" and
// "the app diarizes correctly" are the same statement.
//
// Models: set DIAR_SEG + DIAR_EMB to the segmentation `model.onnx` and the
// embedding `.onnx`; otherwise it looks in the app-support diarization dir. If
// neither is present it prints guidance and exits 0 (CI-safe).
//
// Usage:
//   DIAR_SEG=.../model.onnx DIAR_EMB=.../campplus_zh_en_advanced.onnx \
//     cargo run --example diarize_ground_truth [RESULTS.md]

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use handy_app_lib::meeting::diarize::{open_default, permutation_accuracy, GroundTruthSpan};

const SAMPLE_RATE: u32 = 16_000;
/// (speaker label, macOS `say` voice, spoken text). Round-robin so every speaker
/// gets two contiguous ~12 s turns — enough per-speaker audio to cluster well.
const TURNS: &[(&str, &str, &str)] = &[
    ("A", "Daniel", "Good morning everyone, thanks for joining the weekly sync. Let us get started with the status updates from each of the teams, and then we can talk through the rollout plan in detail before we finish."),
    ("B", "Samantha", "Thanks so much. On my side the database migration is almost complete. I finished the new schema yesterday afternoon, and I already prepared the monitoring dashboards so we can watch error rates during the launch."),
    ("C", "Rishi", "That is really great to hear from both of you. I do have one question about the rollout plan. Are we planning to ship the update to every region at once, or should we consider a slower staggered approach instead?"),
    ("A", "Daniel", "Excellent work all around. Let us make sure the documentation is fully updated before we flip the switch on production, and remember to double check the monitoring dashboards one final time tonight."),
    ("B", "Samantha", "I already drafted the release notes for this version. I will share them in the shared channel right after this meeting so everyone can review the details and add any missing items before we publish them."),
    ("C", "Rishi", "One more thing before we wrap up. Can we schedule a quick retrospective next week so the whole team can capture the lessons we learned from this launch while everything is still fresh in our minds together?"),
];

fn main() {
    println!("== OpenFlow Meetings M2 — diarization ground-truth harness ==\n");

    let (seg_model, emb_model) = match resolve_models() {
        Some(p) => p,
        None => {
            eprintln!(
                "Diarization models not found. Set DIAR_SEG and DIAR_EMB, or install them via\n\
                 the app (Settings → Meetings → enable diarization). Skipping (exit 0)."
            );
            return;
        }
    };
    println!("segmentation: {}", seg_model.display());
    println!("embedding:    {}\n", emb_model.display());

    // 1. Synthesize the deterministic conversation.
    let tmp = std::env::temp_dir().join("openflow_diar_gt");
    let _ = std::fs::create_dir_all(&tmp);
    let (samples, truth) = match synthesize(&tmp) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("audio synthesis failed ({e}); is this macOS with `say`/`afconvert`? Skipping.");
            return;
        }
    };
    let total_s = samples.len() as f32 / SAMPLE_RATE as f32;
    println!("synthesized {:.1}s, {} turns, 3 speakers\n", total_s, truth.len());

    // 2. Run the SAME engine the app uses.
    let engine = open_default(&seg_model, &emb_model).expect("load diarization engine");
    let t0 = Instant::now();
    let turns = engine.diarize(&samples).expect("diarize");
    let proc_ms = t0.elapsed().as_millis();
    let ratio_rt = proc_ms as f32 / 1000.0 / total_s;

    let detected: std::collections::BTreeSet<i64> = turns.iter().map(|t| t.speaker).collect();
    println!("engine: {} speakers, {} turns", detected.len(), turns.len());
    println!(
        "benchmark: {} ms for {:.1}s = {:.3}x realtime (this machine)\n",
        proc_ms, total_s, ratio_rt
    );

    // 3. Score with the real, unit-tested permutation accuracy.
    let acc = permutation_accuracy(&truth, &turns, 10);
    let pct = acc * 100.0;

    // A display-only cluster→speaker mapping (dominant true label per cluster).
    let mapping = dominant_mapping(&truth, &turns);
    println!("permutation map (predicted cluster → dominant true speaker):");
    for (cluster, label) in &mapping {
        println!("  cluster {cluster} → {label}");
    }
    println!(
        "\nACCURACY = {pct:.1}%  ({})",
        if pct >= 90.0 { "PASS ≥90%" } else { "FAIL <90%" }
    );

    if let Some(out) = std::env::args().nth(1) {
        let md = results_markdown(total_s, proc_ms, ratio_rt, detected.len(), &mapping, pct);
        if std::fs::write(&out, md).is_ok() {
            println!("\nwrote results to {out}");
        }
    }
    if pct < 90.0 {
        std::process::exit(1);
    }
}

/// Synthesize the conversation to a single 16 kHz mono buffer, returning it plus
/// the exact per-turn ground-truth spans (in `meeting::diarize::GroundTruthSpan`,
/// with a stable integer label per speaker letter).
fn synthesize(tmp: &std::path::Path) -> std::io::Result<(Vec<f32>, Vec<GroundTruthSpan>)> {
    let gap = (0.6 * SAMPLE_RATE as f32) as usize; // 600 ms silence between turns
    let mut samples: Vec<f32> = Vec::new();
    let mut truth: Vec<GroundTruthSpan> = Vec::new();
    let label_id = |l: &str| -> i64 { (l.bytes().next().unwrap_or(b'?') - b'A') as i64 };
    for (i, (label, voice, text)) in TURNS.iter().enumerate() {
        let aiff = tmp.join(format!("t{i}.aiff"));
        let wav = tmp.join(format!("t{i}.wav"));
        if !Command::new("say")
            .args(["-v", voice, "-r", "175", "-o"])
            .arg(&aiff)
            .arg(text)
            .status()?
            .success()
        {
            return Err(std::io::Error::other(format!("say failed for {voice}")));
        }
        if !Command::new("afconvert")
            .args(["-f", "WAVE", "-d", "LEI16@16000", "-c", "1"])
            .arg(&aiff)
            .arg(&wav)
            .status()?
            .success()
        {
            return Err(std::io::Error::other("afconvert failed"));
        }
        let mut reader = hound::WavReader::open(&wav).map_err(std::io::Error::other)?;
        let turn: Vec<f32> = reader
            .samples::<i16>()
            .map(|s| s.unwrap_or(0) as f32 / i16::MAX as f32)
            .collect();
        let start_ms = (samples.len() as i64) * 1000 / SAMPLE_RATE as i64;
        samples.extend_from_slice(&turn);
        let end_ms = (samples.len() as i64) * 1000 / SAMPLE_RATE as i64;
        truth.push(GroundTruthSpan {
            start_ms,
            end_ms,
            label: label_id(label),
        });
        samples.extend(std::iter::repeat(0.0).take(gap));
        let _ = std::fs::remove_file(&aiff);
        let _ = std::fs::remove_file(&wav);
    }
    Ok((samples, truth))
}

/// Display-only: each predicted cluster → the true speaker letter it covers most.
fn dominant_mapping(
    truth: &[GroundTruthSpan],
    turns: &[handy_app_lib::meeting::diarize::DiarTurn],
) -> Vec<(i64, String)> {
    let letter = |id: i64| ((b'A' + id as u8) as char).to_string();
    let mut counts: HashMap<i64, HashMap<i64, i64>> = HashMap::new();
    let mut t = 0i64;
    let end = truth.iter().map(|s| s.end_ms).max().unwrap_or(0);
    while t < end {
        let tl = truth
            .iter()
            .find(|s| s.start_ms <= t && t < s.end_ms)
            .map(|s| s.label);
        let cl = turns
            .iter()
            .find(|tr| tr.start_ms <= t && t < tr.end_ms)
            .map(|tr| tr.speaker);
        if let (Some(tl), Some(cl)) = (tl, cl) {
            *counts.entry(cl).or_default().entry(tl).or_default() += 1;
        }
        t += 10;
    }
    let mut out: Vec<(i64, String)> = counts
        .into_iter()
        .map(|(cl, m)| {
            let best = m.into_iter().max_by_key(|(_, c)| *c).map(|(l, _)| l).unwrap_or(-1);
            (cl, letter(best))
        })
        .collect();
    out.sort();
    out
}

/// Locate the segmentation + embedding models: env first, then the app-support
/// diarization dir.
fn resolve_models() -> Option<(PathBuf, PathBuf)> {
    if let (Ok(seg), Ok(emb)) = (std::env::var("DIAR_SEG"), std::env::var("DIAR_EMB")) {
        let (seg, emb) = (PathBuf::from(seg), PathBuf::from(emb));
        if seg.is_file() && emb.is_file() {
            return Some((seg, emb));
        }
    }
    let home = std::env::var("HOME").ok()?;
    for id in ["knotie.ai.openflow", "com.openflow.app", "openflow"] {
        let dir = PathBuf::from(&home)
            .join("Library/Application Support")
            .join(id)
            .join("models/diarization");
        let seg = dir.join("sherpa-onnx-pyannote-segmentation-3-0/model.onnx");
        let emb = dir.join("campplus_zh_en_advanced.onnx");
        if seg.is_file() && emb.is_file() {
            return Some((seg, emb));
        }
    }
    None
}

fn results_markdown(
    total_s: f32,
    proc_ms: u128,
    ratio_rt: f32,
    num_speakers: usize,
    mapping: &[(i64, String)],
    pct: f64,
) -> String {
    let mut s = String::new();
    s.push_str("# M2 diarization — ground-truth harness result\n\n");
    s.push_str(&format!(
        "- Audio: {total_s:.1}s synthesized, 3 speakers (Daniel/Samantha/Rishi)\n"
    ));
    s.push_str("- Engine: pyannote-seg-3.0 + CAM++ zh_en advanced, threshold 0.7, auto count\n");
    s.push_str(&format!("- Detected speakers: {num_speakers}\n"));
    s.push_str(&format!("- Wall time: {proc_ms} ms ({ratio_rt:.3}x realtime)\n"));
    s.push_str(&format!(
        "- **Accuracy: {pct:.1}%** ({})\n\n",
        if pct >= 90.0 { "PASS" } else { "FAIL" }
    ));
    s.push_str("| predicted cluster | dominant true speaker |\n|---|---|\n");
    for (c, l) in mapping {
        s.push_str(&format!("| {c} | {l} |\n"));
    }
    s
}
