// macOS-only hardware probe — real implementation lives in
// probe_support/meeting_detect_probe_impl.rs (docs there). This wrapper exists so the
// example still compiles on non-macOS CI (a crate-level cfg would strip the
// whole file, leaving no `main` and failing the build with E0601).
#[cfg(target_os = "macos")]
include!("probe_support/meeting_detect_probe_impl.rs");

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("meeting_detect_probe is a macOS-only probe; nothing to do on this platform.");
}
