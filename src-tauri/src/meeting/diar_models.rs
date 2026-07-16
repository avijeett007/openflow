//! Download + on-disk resolution of the M2 diarization models.
//!
//! Mirrors the STT model-manager conventions (progress events + sha256
//! verification, DESIGN-meetings.md §1) but stays a small, self-contained
//! sibling rather than an entry in the STT catalog — diarization models are not
//! `EngineType` STT models and must never appear in the model picker. They are
//! **never** fetched at startup: the download runs only when the user enables
//! diarization / taps "Download models" (§ non-negotiable — no silent 30 MB
//! downloads at startup), and streams `diarization-model-progress` for the card.
//!
//! Two assets, both from the official k2-fsa release pages (verified 2026-07-16):
//! - pyannote **segmentation-3.0** (~6 MB, MIT) — shipped only as a `.tar.bz2`
//!   that also carries its LICENSE, which we keep beside the model.
//! - 3D-Speaker **CAM++** `zh_en` advanced embeddings (~28 MB, Apache-2.0). The
//!   ground-truth harness measured this export at ~99.7 % vs. ~69 % for the
//!   `en_voxceleb` variant the design first named, so we ship the stronger one
//!   (§5.3 says "verify at integration; never hardcode" — the doc deferred to
//!   measurement, and this is it).
//!
//! macOS-only, matching the capture path and the sherpa native engine.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use log::info;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specta::Type;
use tauri::{AppHandle, Emitter};

/// A diarization model asset to fetch + verify.
struct DiarAsset {
    /// Stable id used in progress events.
    id: &'static str,
    url: &'static str,
    sha256: &'static str,
    size: u64,
    /// Downloaded file name under the diarization dir.
    download_name: &'static str,
    /// `true` → a `.tar.bz2` archive to extract; `false` → a raw file.
    is_archive: bool,
    /// Path (relative to the diarization dir) of the model file we ultimately
    /// resolve — for the archive, the file inside it.
    resolved_rel: &'static str,
}

const SEGMENTATION: DiarAsset = DiarAsset {
    id: "segmentation",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2",
    sha256: "24615ee884c897d9d2ba09bb4d30da6bb1b15e685065962db5b02e76e4996488",
    size: 6_958_444,
    download_name: "sherpa-onnx-pyannote-segmentation-3-0.tar.bz2",
    is_archive: true,
    resolved_rel: "sherpa-onnx-pyannote-segmentation-3-0/model.onnx",
};

const EMBEDDING: DiarAsset = DiarAsset {
    id: "embedding",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx",
    sha256: "aa3cfc16963a10586a9393f5035d6d6b57e98d358b347f80c2a30bf4f00ceba2",
    size: 28_281_164,
    download_name: "campplus_zh_en_advanced.onnx",
    is_archive: false,
    resolved_rel: "campplus_zh_en_advanced.onnx",
};

const ASSETS: [DiarAsset; 2] = [SEGMENTATION, EMBEDDING];

/// Progress for the settings model-download card. Emitted raw as
/// `diarization-model-progress` (the frontend listens with a plain `listen`).
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct DiarizationModelProgress {
    /// `segmentation` | `embedding` | `done` | `error`.
    pub stage: String,
    pub downloaded: u64,
    pub total: u64,
    pub percentage: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `app_data_dir/models/diarization`.
pub fn diarization_dir(app: &AppHandle) -> Result<PathBuf> {
    let dir = crate::portable::app_data_dir(app)
        .map_err(|e| anyhow!("app data dir: {e}"))?
        .join("models")
        .join("diarization");
    Ok(dir)
}

/// Resolve the (segmentation, embedding) model paths iff both are installed.
pub fn resolve_model_paths(app: &AppHandle) -> Option<(PathBuf, PathBuf)> {
    let dir = diarization_dir(app).ok()?;
    let seg = dir.join(SEGMENTATION.resolved_rel);
    let emb = dir.join(EMBEDDING.resolved_rel);
    (seg.is_file() && emb.is_file()).then_some((seg, emb))
}

/// Are both models present on disk?
pub fn models_installed(app: &AppHandle) -> bool {
    resolve_model_paths(app).is_some()
}

pub fn total_size_mb() -> u64 {
    ASSETS.iter().map(|a| a.size).sum::<u64>() / (1024 * 1024)
}

/// Download and verify both models (skipping any already installed), extracting
/// the segmentation archive. Streams progress; the terminal event has
/// `stage = "done"` or `stage = "error"`.
pub async fn download_all(app: &AppHandle) -> Result<()> {
    let dir = diarization_dir(app)?;
    std::fs::create_dir_all(&dir).context("create diarization dir")?;

    for asset in ASSETS.iter() {
        let resolved = dir.join(asset.resolved_rel);
        if resolved.is_file() {
            info!("diarization: {} already installed", asset.id);
            continue;
        }
        if let Err(e) = download_asset(app, &dir, asset).await {
            let _ = DiarizationModelProgress {
                stage: "error".into(),
                downloaded: 0,
                total: asset.size,
                percentage: 0.0,
                error: Some(e.to_string()),
            }
            .emit_to_frontend(app);
            return Err(e);
        }
    }

    let _ = DiarizationModelProgress {
        stage: "done".into(),
        downloaded: total_bytes(),
        total: total_bytes(),
        percentage: 100.0,
        error: None,
    }
    .emit_to_frontend(app);
    info!("diarization: all models installed");
    Ok(())
}

fn total_bytes() -> u64 {
    ASSETS.iter().map(|a| a.size).sum()
}

async fn download_asset(app: &AppHandle, dir: &Path, asset: &DiarAsset) -> Result<()> {
    let download_path = dir.join(asset.download_name);
    info!("diarization: downloading {} from {}", asset.id, asset.url);

    let response = reqwest::get(asset.url)
        .await
        .with_context(|| format!("GET {}", asset.url))?;
    if !response.status().is_success() {
        bail!("download {} failed: HTTP {}", asset.id, response.status());
    }
    let total = response.content_length().unwrap_or(asset.size);

    let mut file = std::fs::File::create(&download_path)
        .with_context(|| format!("create {}", download_path.display()))?;
    let mut downloaded: u64 = 0;
    let mut stream = response.bytes_stream();
    let mut last_emit = std::time::Instant::now();

    let _ = DiarizationModelProgress {
        stage: asset.id.into(),
        downloaded: 0,
        total,
        percentage: 0.0,
        error: None,
    }
    .emit_to_frontend(app);

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("download stream error")?;
        file.write_all(&chunk).context("write chunk")?;
        downloaded += chunk.len() as u64;
        if last_emit.elapsed() >= std::time::Duration::from_millis(100) {
            let _ = DiarizationModelProgress {
                stage: asset.id.into(),
                downloaded,
                total,
                percentage: if total > 0 {
                    downloaded as f64 / total as f64 * 100.0
                } else {
                    0.0
                },
                error: None,
            }
            .emit_to_frontend(app);
            last_emit = std::time::Instant::now();
        }
    }
    file.flush().ok();
    drop(file);

    verify_sha256(&download_path, asset.sha256)
        .with_context(|| format!("checksum {}", asset.id))?;

    if asset.is_archive {
        extract_tar_bz2(&download_path, dir).with_context(|| format!("extract {}", asset.id))?;
        let _ = std::fs::remove_file(&download_path);
    } else if asset.download_name != asset.resolved_rel {
        // Direct file: the download name IS the resolved name here, so nothing
        // to move. (Kept explicit in case a future asset differs.)
    }
    Ok(())
}

fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    let got = hex_lower(&hasher.finalize());
    if got != expected {
        // Remove the corrupt file so a retry re-downloads cleanly.
        let _ = std::fs::remove_file(path);
        bail!("sha256 mismatch: expected {expected}, got {got}");
    }
    Ok(())
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn extract_tar_bz2(archive: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)?;
    let decoder = bzip2::read::BzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);
    tar.unpack(dest)?;
    Ok(())
}

impl DiarizationModelProgress {
    fn emit_to_frontend(&self, app: &AppHandle) -> Result<()> {
        app.emit("diarization-model-progress", self)
            .map_err(|e| anyhow!("emit: {e}"))
    }
}
