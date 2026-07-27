//! Per-CLI session adapters: capture a session id from a run's streamed output,
//! and rewrite the argv to resume a given session.
//!
//! Pure functions keyed by `AgentCliType`. The behavior matrix is normative in
//! `documentation/design/session-hotkeys/DESIGN.md` ("Per-CLI adapter matrix");
//! wire details in `documentation/research/cli-session-resume.md`.

use crate::settings::AgentCliType;

/// Whether this CLI supports session capture/resume (false for `Custom`).
pub fn supports_sessions(cli: AgentCliType) -> bool {
    !matches!(cli, AgentCliType::Custom)
}

/// Try to extract a session id from one streamed output line.
///
/// `is_stderr` distinguishes the two streams — Hermes prints its id on **stderr**;
/// Claude/Codex/Kimi emit structured JSON on **stdout**. OpenClaw and Custom never
/// emit an id (OpenClaw ids are caller-generated).
pub fn extract_session_id(cli: AgentCliType, line: &str, is_stderr: bool) -> Option<String> {
    match cli {
        // Claude Code stream-json: the init/system event carries a string session_id.
        AgentCliType::Claude => {
            if is_stderr {
                return None;
            }
            json_string_field(line, "session_id")
        }
        // Codex --json: {"type":"thread.started","thread_id":"…"}
        AgentCliType::Codex => {
            if is_stderr {
                return None;
            }
            json_field_when_type(line, "thread.started", "thread_id")
        }
        // Kimi stream-json meta: {"type":"session.resume_hint","session_id":"…"}
        AgentCliType::Kimi => {
            if is_stderr {
                return None;
            }
            json_field_when_type(line, "session.resume_hint", "session_id")
        }
        // Hermes prints `session_id: <id>` on stderr only.
        AgentCliType::Hermes => {
            if !is_stderr {
                return None;
            }
            line.trim()
                .strip_prefix("session_id:")
                .map(|rest| rest.trim().to_string())
                .filter(|s| !s.is_empty())
        }
        // OpenClaw returns no id (caller generates it); Custom unsupported.
        AgentCliType::Openclaw | AgentCliType::Custom => None,
    }
}

/// Rewrite the template argv to resume `session_id`. For CLIs that don't support
/// sessions (`Custom`), the argv is returned unchanged.
pub fn resume_argv(cli: AgentCliType, argv: Vec<String>, session_id: &str) -> Vec<String> {
    match cli {
        AgentCliType::Claude | AgentCliType::Hermes => append(argv, &["--resume", session_id]),
        AgentCliType::Kimi => append(argv, &["-r", session_id]),
        AgentCliType::Openclaw => append(argv, &["--session-id", session_id]),
        AgentCliType::Codex => insert_after_exec(argv, session_id),
        AgentCliType::Custom => argv,
    }
}

/// Parse `line` as a JSON object and return the string value at `field`, if present.
fn json_string_field(line: &str, field: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    v.get(field)?.as_str().map(|s| s.to_string())
}

/// Return `field` (string) only when the object's `type` equals `want_type`.
fn json_field_when_type(line: &str, want_type: &str, field: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    if v.get("type").and_then(|t| t.as_str()) != Some(want_type) {
        return None;
    }
    v.get(field)?.as_str().map(|s| s.to_string())
}

fn append(mut argv: Vec<String>, extra: &[&str]) -> Vec<String> {
    argv.extend(extra.iter().map(|s| s.to_string()));
    argv
}

/// Insert `resume <id>` immediately after the first `exec` token; if there is no
/// `exec` token, append `resume <id>` at the end.
fn insert_after_exec(argv: Vec<String>, session_id: &str) -> Vec<String> {
    let mut out = Vec::with_capacity(argv.len() + 2);
    match argv.iter().position(|a| a == "exec") {
        Some(i) => {
            out.extend_from_slice(&argv[..=i]);
            out.push("resume".to_string());
            out.push(session_id.to_string());
            out.extend_from_slice(&argv[i + 1..]);
        }
        None => {
            out.extend(argv);
            out.push("resume".to_string());
            out.push(session_id.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    // ---- Claude ----
    #[test]
    fn claude_extracts_session_id_from_stream_json_init() {
        let line = r#"{"type":"system","subtype":"init","session_id":"8f2f6f4e-1234","model":"x"}"#;
        assert_eq!(
            extract_session_id(AgentCliType::Claude, line, false),
            Some("8f2f6f4e-1234".into())
        );
        assert_eq!(extract_session_id(AgentCliType::Claude, "not json", false), None);
        // stderr line is ignored for Claude
        assert_eq!(extract_session_id(AgentCliType::Claude, line, true), None);
    }

    #[test]
    fn claude_resume_appends_flag() {
        assert_eq!(
            resume_argv(AgentCliType::Claude, v(&["-p", "{prompt}"]), "sid-1"),
            v(&["-p", "{prompt}", "--resume", "sid-1"])
        );
    }

    // ---- Codex ----
    #[test]
    fn codex_extracts_thread_id_on_thread_started() {
        let line = r#"{"type":"thread.started","thread_id":"tid-1"}"#;
        assert_eq!(
            extract_session_id(AgentCliType::Codex, line, false),
            Some("tid-1".into())
        );
        // wrong type → None
        let other = r#"{"type":"turn.completed","thread_id":"tid-1"}"#;
        assert_eq!(extract_session_id(AgentCliType::Codex, other, false), None);
    }

    #[test]
    fn codex_resume_inserts_after_exec_token() {
        assert_eq!(
            resume_argv(AgentCliType::Codex, v(&["exec", "--json", "{prompt}"]), "tid-1"),
            v(&["exec", "resume", "tid-1", "--json", "{prompt}"])
        );
    }

    #[test]
    fn codex_resume_appends_when_no_exec_token() {
        assert_eq!(
            resume_argv(AgentCliType::Codex, v(&["--json", "{prompt}"]), "tid-1"),
            v(&["--json", "{prompt}", "resume", "tid-1"])
        );
    }

    // ---- Kimi ----
    #[test]
    fn kimi_extracts_from_resume_hint_meta() {
        let line = r#"{"role":"meta","type":"session.resume_hint","session_id":"k-9","command":"kimi -r k-9"}"#;
        assert_eq!(
            extract_session_id(AgentCliType::Kimi, line, false),
            Some("k-9".into())
        );
    }

    #[test]
    fn kimi_resume_appends_dash_r() {
        assert_eq!(
            resume_argv(AgentCliType::Kimi, v(&["-p", "{prompt}", "--output-format", "text"]), "k-9"),
            v(&["-p", "{prompt}", "--output-format", "text", "-r", "k-9"])
        );
    }

    // ---- Hermes ----
    #[test]
    fn hermes_extracts_from_stderr_only() {
        assert_eq!(
            extract_session_id(AgentCliType::Hermes, "session_id: 20260727_101010_ab", true),
            Some("20260727_101010_ab".into())
        );
        // same line on stdout is ignored
        assert_eq!(
            extract_session_id(AgentCliType::Hermes, "session_id: x", false),
            None
        );
        // empty id → None
        assert_eq!(extract_session_id(AgentCliType::Hermes, "session_id:", true), None);
    }

    #[test]
    fn hermes_resume_appends_flag() {
        assert_eq!(
            resume_argv(AgentCliType::Hermes, v(&["-z", "{prompt}", "--yolo"]), "sid"),
            v(&["-z", "{prompt}", "--yolo", "--resume", "sid"])
        );
    }

    // ---- OpenClaw ----
    #[test]
    fn openclaw_never_extracts_and_resume_appends_session_id() {
        assert_eq!(
            extract_session_id(AgentCliType::Openclaw, "session_id: whatever", true),
            None
        );
        assert_eq!(
            resume_argv(
                AgentCliType::Openclaw,
                v(&["agent", "--local", "--agent", "main", "--message", "{prompt}"]),
                "uuid-1"
            ),
            v(&[
                "agent", "--local", "--agent", "main", "--message", "{prompt}",
                "--session-id", "uuid-1"
            ])
        );
    }

    // ---- Custom ----
    #[test]
    fn custom_unsupported_and_argv_unchanged() {
        assert!(!supports_sessions(AgentCliType::Custom));
        assert!(supports_sessions(AgentCliType::Claude));
        assert_eq!(
            resume_argv(AgentCliType::Custom, v(&["{prompt}"]), "x"),
            v(&["{prompt}"])
        );
        assert_eq!(extract_session_id(AgentCliType::Custom, "anything", false), None);
    }
}
