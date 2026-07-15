//! Best-effort detection of the frontmost application, its window title, and an
//! inferred project name. Used by the analytics backend (M4) to attribute each
//! dictation to the app/project the user was working in.
//!
//! Every platform path is defensive: on any failure we fall back to
//! `{ app_name: "unknown", window_title: None, project: None }` so callers can
//! log unconditionally and never block the dictation pipeline.

use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct ActiveApp {
    pub app_name: String,
    pub window_title: Option<String>,
    pub project: Option<String>,
}

impl Default for ActiveApp {
    fn default() -> Self {
        ActiveApp {
            app_name: "unknown".to_string(),
            window_title: None,
            project: None,
        }
    }
}

/// Return the current frontmost application (best effort).
pub fn current() -> ActiveApp {
    #[cfg(target_os = "macos")]
    {
        current_macos()
    }
    #[cfg(windows)]
    {
        current_windows()
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        ActiveApp::default()
    }
}

#[cfg(target_os = "macos")]
fn current_macos() -> ActiveApp {
    // Read the frontmost app name via NSWorkspace. Unlike the old
    // System Events / osascript path (which needs an Accessibility grant and
    // otherwise errors with -1719), NSWorkspace.frontmostApplication requires no
    // special permission. Any failure degrades to `unknown`.
    let app_name = frontmost_app_name()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    // Window title via AX is optional and finicky; try a best-effort read and
    // leave it None on any failure. This is the only remaining osascript use and
    // is non-fatal — the app name no longer depends on it.
    let window_title = run_osascript(
        "tell application \"System Events\" to tell (first process whose frontmost is true) to get value of attribute \"AXTitle\" of front window",
    )
    .filter(|s| !s.is_empty() && s != "missing value");

    let project = window_title.as_deref().and_then(infer_project);

    ActiveApp {
        app_name,
        window_title,
        project,
    }
}

/// The frontmost application's bundle identifier and localized name via
/// NSWorkspace (no Accessibility permission required). Returns `None` when there
/// is no frontmost app or it has no bundle id (e.g. some system UI elements).
///
/// Used by the meeting-capture hotkey to target the system-audio tap at whatever
/// app the user is calling in — crucially browsers, since Google Meet runs in a
/// tab and can never be caught by the bundle-id auto-detection allowlist.
#[cfg(target_os = "macos")]
pub fn frontmost_bundle() -> Option<(String, String)> {
    use objc2_app_kit::NSWorkspace;

    let workspace = NSWorkspace::sharedWorkspace();
    let app = workspace.frontmostApplication()?;
    let bundle = app.bundleIdentifier()?.to_string();
    if bundle.is_empty() {
        return None;
    }
    let name = app
        .localizedName()
        .map(|n| n.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| bundle.clone());
    Some((bundle, name))
}

/// Non-macOS stub: there is no system-audio process tap outside macOS, so the
/// meeting-capture hotkey always starts mic-only.
#[cfg(not(target_os = "macos"))]
pub fn frontmost_bundle() -> Option<(String, String)> {
    None
}

/// Frontmost application's localized name via NSWorkspace (no Accessibility
/// permission required). Returns `None` when there is no frontmost app or the
/// name is nil.
#[cfg(target_os = "macos")]
fn frontmost_app_name() -> Option<String> {
    use objc2_app_kit::NSWorkspace;

    // NSWorkspace.sharedWorkspace + frontmostApplication/localizedName are
    // thread-safe read-only accessors exposed as safe by objc2-app-kit; all
    // returned objects are retained.
    let workspace = NSWorkspace::sharedWorkspace();
    let app = workspace.frontmostApplication()?;
    let name = app.localizedName()?;
    let s = name.to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Run an AppleScript snippet with a short timeout, returning trimmed stdout.
#[cfg(target_os = "macos")]
fn run_osascript(script: &str) -> Option<String> {
    use std::process::{Command, Stdio};
    use std::time::Duration;

    let mut child = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    // Poll for completion with a short timeout so a hung AppleScript never
    // stalls the analytics logging path.
    let deadline = std::time::Instant::now() + Duration::from_millis(800);
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    }

    let output = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

#[cfg(windows)]
fn current_windows() -> ActiveApp {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW};

    let window_title = unsafe {
        let hwnd: HWND = GetForegroundWindow();
        if hwnd.0.is_null() {
            None
        } else {
            let mut buf = [0u16; 512];
            let len = GetWindowTextW(hwnd, &mut buf);
            if len > 0 {
                Some(String::from_utf16_lossy(&buf[..len as usize]))
            } else {
                None
            }
        }
    }
    .filter(|s: &String| !s.is_empty());

    // We don't resolve the process image name here (that needs extra features);
    // derive an app name from the window title heuristics, falling back to unknown.
    let app_name = window_title
        .as_deref()
        .and_then(app_name_from_title)
        .unwrap_or_else(|| "unknown".to_string());

    let project = window_title.as_deref().and_then(infer_project);

    ActiveApp {
        app_name,
        window_title,
        project,
    }
}

/// Common application suffixes that appear in window titles, e.g.
/// "main.rs - project - Visual Studio Code".
const APP_SUFFIXES: &[&str] = &[
    "Google Chrome",
    "Chromium",
    "Mozilla Firefox",
    "Firefox",
    "Microsoft Edge",
    "Safari",
    "Visual Studio Code",
    "VSCodium",
    "Cursor",
    "Sublime Text",
    "IntelliJ IDEA",
    "PyCharm",
    "WebStorm",
    "Xcode",
    "iTerm2",
    "iTerm",
    "Terminal",
    "Slack",
    "Discord",
    "Notion",
    "Obsidian",
];

/// On Windows we lack a process name, so guess the app from the title's trailing
/// suffix (many apps append " - <App Name>").
#[cfg(windows)]
fn app_name_from_title(title: &str) -> Option<String> {
    for suffix in APP_SUFFIXES {
        if title.ends_with(suffix) {
            return Some((*suffix).to_string());
        }
    }
    None
}

/// Split points commonly used by apps between a document/segment and the app or
/// context name.
const SEGMENT_SEPARATORS: &[&str] = &[" — ", " - ", " | ", " – "];

/// Infer a project/repo name from a window title. Deliberately conservative:
/// returns None when nothing sensible can be extracted.
pub fn infer_project(title: &str) -> Option<String> {
    let title = title.trim();
    if title.is_empty() {
        return None;
    }

    // Break into segments on common separators.
    let mut segments: Vec<String> = vec![title.to_string()];
    for sep in SEGMENT_SEPARATORS {
        segments = segments
            .into_iter()
            .flat_map(|s| {
                s.split(sep)
                    .map(|p| p.trim().to_string())
                    .collect::<Vec<_>>()
            })
            .collect();
    }
    segments.retain(|s| !s.is_empty());
    if segments.is_empty() {
        return None;
    }

    // Drop segments that are just a known application name.
    let is_app_suffix = |s: &str| {
        APP_SUFFIXES
            .iter()
            .any(|suffix| s.eq_ignore_ascii_case(suffix))
    };
    let candidates: Vec<String> = segments
        .iter()
        .filter(|s| !is_app_suffix(s))
        .cloned()
        .collect();
    let candidates = if candidates.is_empty() {
        segments
    } else {
        candidates
    };

    // Prefer a segment that looks like a path — take its last directory
    // component (a good proxy for a repo/project folder).
    for seg in &candidates {
        if seg.contains('/') || seg.contains('\\') {
            let last = seg.split(['/', '\\']).filter(|p| !p.is_empty()).next_back();
            if let Some(name) = last {
                let name = strip_app_suffixes(name);
                if is_sensible_project(&name) {
                    return Some(name);
                }
            }
        }
    }

    // Otherwise prefer the last non-app segment (editors often place the project
    // name just before the app name), then fall back to the first.
    for seg in candidates.iter().rev() {
        let name = strip_app_suffixes(seg);
        if is_sensible_project(&name) {
            return Some(name);
        }
    }

    None
}

fn strip_app_suffixes(s: &str) -> String {
    let mut out = s.trim().to_string();
    for suffix in APP_SUFFIXES {
        let needle = format!("- {}", suffix);
        if let Some(idx) = out.rfind(&needle) {
            out = out[..idx].trim().to_string();
        }
    }
    out.trim().to_string()
}

/// Heuristic guard: reject empty, over-long, or clearly-non-project strings.
fn is_sensible_project(s: &str) -> bool {
    let s = s.trim();
    if s.len() < 2 || s.chars().count() > 60 {
        return false;
    }
    // Reject strings that are mostly whitespace/punctuation.
    let alnum = s.chars().filter(|c| c.is_alphanumeric()).count();
    alnum >= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_project_from_vscode_title() {
        assert_eq!(
            infer_project("main.rs — openflow — Visual Studio Code").as_deref(),
            Some("openflow")
        );
    }

    #[test]
    fn infers_project_from_path_segment() {
        assert_eq!(
            infer_project("~/code/openflow/src — nvim").as_deref(),
            Some("src")
        );
    }

    #[test]
    fn returns_none_for_empty() {
        assert_eq!(infer_project("   "), None);
    }
}
