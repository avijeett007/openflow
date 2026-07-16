// macOS-only diarization ground-truth harness — real implementation lives in
// probe_support/diarize_ground_truth_impl.rs. This wrapper keeps the example
// compiling on non-macOS CI (a crate-level cfg would strip the whole file and
// fail with E0601 "no main"). The #18 lesson: stub `main` off-macOS.
#[cfg(target_os = "macos")]
include!("probe_support/diarize_ground_truth_impl.rs");

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!(
        "diarize_ground_truth is a macOS-only harness (needs `say`/`afconvert` and the \
         sherpa-onnx engine); nothing to do on this platform."
    );
}
