//! OpenFlow usage analytics (M4).
//!
//! `AnalyticsManager` opens the *same* `history.db` SQLite file that
//! [`crate::managers::history::HistoryManager`] owns and runs migrations on at
//! startup. By the time any analytics query executes the `dictation_events`
//! table exists; inserts still guard defensively so a missing table can never
//! panic the dictation pipeline.
//!
//! All inserts respect the user's [`AnalyticsPrivacy`] setting: in
//! `KeywordsOnly` the raw/cleaned text and window title are nulled but derived
//! keywords are kept; in `Off` nothing is logged (the caller skips `log_event`,
//! and `log_event` also guards).

use anyhow::Result;
use chrono::{DateTime, Datelike, Local, TimeZone, Utc};
use log::{debug, warn};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashMap;
use std::path::PathBuf;
use tauri::AppHandle;

use crate::settings::AnalyticsPrivacy;

/// Typing-speed baseline (words per minute) used to estimate time saved by
/// dictating instead of typing.
const TYPING_WPM_BASELINE: f64 = 40.0;

/// A single dictation event to persist. The caller fills every field; privacy
/// filtering (nulling text/title) is applied inside [`AnalyticsManager::log_event`].
#[derive(Clone, Debug)]
pub struct DictationEvent {
    pub ts: i64,
    pub duration_ms: i64,
    pub audio_ms: i64,
    pub word_count: i64,
    pub wpm: f64,
    pub raw_text: Option<String>,
    pub cleaned_text: Option<String>,
    pub active_app: String,
    pub window_title: Option<String>,
    pub detected_project: Option<String>,
    pub language: String,
    pub stt_backend: String,
    pub stt_model: String,
    pub cleanup_backend: String,
    pub cleanup_model: String,
    pub stt_latency_ms: i64,
    pub cleanup_latency_ms: i64,
    pub total_latency_ms: i64,
    pub injected_ok: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct AnalyticsSummary {
    pub total_dictations: i64,
    pub total_words: i64,
    pub avg_wpm: f64,
    pub time_saved_seconds: f64,
    pub current_streak_days: i64,
    pub active_apps_count: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct OverTimePoint {
    pub date: String,
    pub dictations: i64,
    pub words: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct AppUsage {
    pub app: String,
    pub dictations: i64,
    pub words: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct ProjectUsage {
    pub project: String,
    pub dictations: i64,
    pub words: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct KeywordCount {
    pub keyword: String,
    pub count: i64,
}

pub struct AnalyticsManager {
    db_path: PathBuf,
}

impl AnalyticsManager {
    pub fn new(app_handle: &AppHandle) -> Result<Self> {
        let app_data_dir = crate::portable::app_data_dir(app_handle)?;
        let db_path = app_data_dir.join("history.db");
        Ok(Self { db_path })
    }

    fn conn(&self) -> Result<Connection> {
        Ok(Connection::open(&self.db_path)?)
    }

    /// Compute the inclusive lower-bound unix timestamp for a range in days, or
    /// `None` for "all time". A range of `N` days means "the last N days
    /// including today".
    fn range_start_ts(range_days: Option<i64>) -> Option<i64> {
        let days = range_days?;
        if days <= 0 {
            return None;
        }
        // Start of the day (local) that is (days - 1) before today.
        let now = Local::now();
        let start_day = now.date_naive() - chrono::Duration::days(days - 1);
        let start_dt = start_day.and_hms_opt(0, 0, 0)?;
        Local
            .from_local_datetime(&start_dt)
            .single()
            .map(|dt| dt.timestamp())
    }

    /// Insert one dictation event, applying the privacy filter. Never panics;
    /// errors are logged and swallowed so the dictation pipeline is unaffected.
    pub fn log_event(&self, mut ev: DictationEvent, privacy: AnalyticsPrivacy) {
        if let AnalyticsPrivacy::Off = privacy {
            debug!("Analytics privacy is Off; skipping dictation event");
            return;
        }

        // Derive keywords from the transcript *before* any privacy nulling, so
        // KeywordsOnly still captures them once the text itself is dropped.
        let source = ev
            .cleaned_text
            .as_deref()
            .or(ev.raw_text.as_deref())
            .unwrap_or("");
        let keywords_json = if source.is_empty() {
            None
        } else {
            serde_json::to_string(&extract_keywords(source, 20)).ok()
        };

        if let AnalyticsPrivacy::KeywordsOnly = privacy {
            // Keep the derived keywords above but never persist the transcript
            // or window title.
            ev.raw_text = None;
            ev.cleaned_text = None;
            ev.window_title = None;
        }

        if let Err(e) = self.insert_event(&ev, keywords_json) {
            warn!("Failed to log dictation analytics event: {}", e);
        }
    }

    fn insert_event(&self, ev: &DictationEvent, keywords_json: Option<String>) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO dictation_events (
                ts, duration_ms, audio_ms, word_count, wpm,
                raw_text, cleaned_text, keywords, active_app, window_title,
                detected_project, language, stt_backend, stt_model,
                cleanup_backend, cleanup_model, stt_latency_ms,
                cleanup_latency_ms, total_latency_ms, injected_ok
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20
            )",
            params![
                ev.ts,
                ev.duration_ms,
                ev.audio_ms,
                ev.word_count,
                ev.wpm,
                ev.raw_text,
                ev.cleaned_text,
                keywords_json,
                ev.active_app,
                ev.window_title,
                ev.detected_project,
                ev.language,
                ev.stt_backend,
                ev.stt_model,
                ev.cleanup_backend,
                ev.cleanup_model,
                ev.stt_latency_ms,
                ev.cleanup_latency_ms,
                ev.total_latency_ms,
                ev.injected_ok as i64,
            ],
        )?;
        debug!("Logged dictation analytics event ({} words)", ev.word_count);
        Ok(())
    }

    /// Summary metrics over an optional day range (None = all time).
    pub fn summary(&self, range_days: Option<i64>) -> Result<AnalyticsSummary> {
        let conn = self.conn()?;
        let start = Self::range_start_ts(range_days);

        // Pull (word_count, wpm) rows for time-saved + averages.
        let (sql, bound): (&str, Vec<i64>) = match start {
            Some(s) => (
                "SELECT COALESCE(word_count, 0), wpm FROM dictation_events WHERE ts >= ?1",
                vec![s],
            ),
            None => (
                "SELECT COALESCE(word_count, 0), wpm FROM dictation_events",
                vec![],
            ),
        };

        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(bound.iter()), |row| {
            let words: i64 = row.get(0)?;
            let wpm: Option<f64> = row.get(1)?;
            Ok((words, wpm))
        })?;

        let mut total_dictations = 0i64;
        let mut total_words = 0i64;
        let mut wpm_sum = 0.0f64;
        let mut wpm_count = 0i64;
        let mut time_saved_seconds = 0.0f64;

        for row in rows {
            let (words, wpm) = row?;
            total_dictations += 1;
            total_words += words;
            if let Some(w) = wpm {
                if w > 0.0 {
                    wpm_sum += w;
                    wpm_count += 1;
                    // time saved = words/typing_baseline - words/dictation_wpm
                    let typing_secs = (words as f64) / TYPING_WPM_BASELINE * 60.0;
                    let dictation_secs = (words as f64) / w * 60.0;
                    let saved = typing_secs - dictation_secs;
                    if saved > 0.0 {
                        time_saved_seconds += saved;
                    }
                }
            }
        }

        let avg_wpm = if wpm_count > 0 {
            wpm_sum / wpm_count as f64
        } else {
            0.0
        };

        // Distinct active apps.
        let active_apps_count: i64 = match start {
            Some(s) => conn.query_row(
                "SELECT COUNT(DISTINCT active_app) FROM dictation_events WHERE ts >= ?1 AND active_app IS NOT NULL AND active_app != ''",
                params![s],
                |r| r.get(0),
            )?,
            None => conn.query_row(
                "SELECT COUNT(DISTINCT active_app) FROM dictation_events WHERE active_app IS NOT NULL AND active_app != ''",
                [],
                |r| r.get(0),
            )?,
        };

        let current_streak_days = self.current_streak_days(&conn)?;

        Ok(AnalyticsSummary {
            total_dictations,
            total_words,
            avg_wpm,
            time_saved_seconds,
            current_streak_days,
            active_apps_count,
        })
    }

    /// Consecutive days (ending today) that have at least one dictation.
    fn current_streak_days(&self, conn: &Connection) -> Result<i64> {
        // Gather the set of local YYYY-MM-DD dates with at least one event.
        let mut stmt = conn.prepare("SELECT ts FROM dictation_events WHERE ts IS NOT NULL")?;
        let rows = stmt.query_map([], |row| row.get::<_, i64>(0))?;

        let mut days: std::collections::HashSet<i64> = std::collections::HashSet::new();
        for ts in rows {
            let ts = ts?;
            if let Some(dt) = DateTime::<Utc>::from_timestamp(ts, 0) {
                let local = dt.with_timezone(&Local);
                // Days since epoch in local time as the streak key.
                let num = local.date_naive().num_days_from_ce() as i64;
                days.insert(num);
            }
        }

        if days.is_empty() {
            return Ok(0);
        }

        let today = Local::now().date_naive().num_days_from_ce() as i64;
        // Streak counts from today backwards. If today has no dictation, the
        // streak is 0 (an unbroken run must include today).
        let mut streak = 0i64;
        let mut cursor = today;
        while days.contains(&cursor) {
            streak += 1;
            cursor -= 1;
        }
        Ok(streak)
    }

    /// Per-day dictation and word counts.
    pub fn over_time(&self, range_days: Option<i64>) -> Result<Vec<OverTimePoint>> {
        let conn = self.conn()?;
        let start = Self::range_start_ts(range_days);

        let (sql, bound): (&str, Vec<i64>) = match start {
            Some(s) => (
                "SELECT ts, COALESCE(word_count, 0) FROM dictation_events WHERE ts >= ?1",
                vec![s],
            ),
            None => (
                "SELECT ts, COALESCE(word_count, 0) FROM dictation_events",
                vec![],
            ),
        };

        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(bound.iter()), |row| {
            let ts: Option<i64> = row.get(0)?;
            let words: i64 = row.get(1)?;
            Ok((ts, words))
        })?;

        // Aggregate by local YYYY-MM-DD.
        let mut buckets: HashMap<String, (i64, i64)> = HashMap::new();
        for row in rows {
            let (ts, words) = row?;
            let Some(ts) = ts else { continue };
            let Some(dt) = DateTime::<Utc>::from_timestamp(ts, 0) else {
                continue;
            };
            let date = dt.with_timezone(&Local).format("%Y-%m-%d").to_string();
            let entry = buckets.entry(date).or_insert((0, 0));
            entry.0 += 1;
            entry.1 += words;
        }

        let mut out: Vec<OverTimePoint> = buckets
            .into_iter()
            .map(|(date, (dictations, words))| OverTimePoint {
                date,
                dictations,
                words,
            })
            .collect();
        out.sort_by(|a, b| a.date.cmp(&b.date));
        Ok(out)
    }

    /// Dictation and word counts grouped by active app.
    pub fn by_app(&self, range_days: Option<i64>) -> Result<Vec<AppUsage>> {
        let conn = self.conn()?;
        let start = Self::range_start_ts(range_days);

        let (sql, bound): (&str, Vec<i64>) = match start {
            Some(s) => (
                "SELECT COALESCE(active_app, 'unknown') AS app,
                        COUNT(*), COALESCE(SUM(word_count), 0)
                 FROM dictation_events WHERE ts >= ?1
                 GROUP BY app ORDER BY COUNT(*) DESC",
                vec![s],
            ),
            None => (
                "SELECT COALESCE(active_app, 'unknown') AS app,
                        COUNT(*), COALESCE(SUM(word_count), 0)
                 FROM dictation_events
                 GROUP BY app ORDER BY COUNT(*) DESC",
                vec![],
            ),
        };

        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(bound.iter()), |row| {
            Ok(AppUsage {
                app: row.get(0)?,
                dictations: row.get(1)?,
                words: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Dictation and word counts grouped by detected project.
    pub fn by_project(&self, range_days: Option<i64>) -> Result<Vec<ProjectUsage>> {
        let conn = self.conn()?;
        let start = Self::range_start_ts(range_days);

        let (sql, bound): (&str, Vec<i64>) = match start {
            Some(s) => (
                "SELECT detected_project, COUNT(*), COALESCE(SUM(word_count), 0)
                 FROM dictation_events
                 WHERE ts >= ?1 AND detected_project IS NOT NULL AND detected_project != ''
                 GROUP BY detected_project ORDER BY COUNT(*) DESC",
                vec![s],
            ),
            None => (
                "SELECT detected_project, COUNT(*), COALESCE(SUM(word_count), 0)
                 FROM dictation_events
                 WHERE detected_project IS NOT NULL AND detected_project != ''
                 GROUP BY detected_project ORDER BY COUNT(*) DESC",
                vec![],
            ),
        };

        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(bound.iter()), |row| {
            Ok(ProjectUsage {
                project: row.get(0)?,
                dictations: row.get(1)?,
                words: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Top keywords across events in range. Reads the per-event `keywords` JSON
    /// arrays and aggregates; falls back to deriving keywords from stored text
    /// when the JSON column is empty.
    pub fn top_keywords(
        &self,
        range_days: Option<i64>,
        limit: Option<i64>,
    ) -> Result<Vec<KeywordCount>> {
        let conn = self.conn()?;
        let start = Self::range_start_ts(range_days);
        let limit = limit.unwrap_or(20).max(1) as usize;

        let (sql, bound): (&str, Vec<i64>) = match start {
            Some(s) => (
                "SELECT keywords, cleaned_text, raw_text FROM dictation_events WHERE ts >= ?1",
                vec![s],
            ),
            None => (
                "SELECT keywords, cleaned_text, raw_text FROM dictation_events",
                vec![],
            ),
        };

        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(bound.iter()), |row| {
            let keywords: Option<String> = row.get(0)?;
            let cleaned: Option<String> = row.get(1)?;
            let raw: Option<String> = row.get(2)?;
            Ok((keywords, cleaned, raw))
        })?;

        let mut counts: HashMap<String, i64> = HashMap::new();
        for row in rows {
            let (keywords, cleaned, raw) = row?;
            let mut used_json = false;
            if let Some(json) = keywords.as_deref() {
                if let Ok(list) = serde_json::from_str::<Vec<String>>(json) {
                    for kw in list {
                        *counts.entry(kw).or_insert(0) += 1;
                    }
                    used_json = true;
                }
            }
            if !used_json {
                // Fallback: derive from whatever text we retained.
                let source = cleaned.as_deref().or(raw.as_deref()).unwrap_or("");
                if !source.is_empty() {
                    for kw in extract_keywords(source, 20) {
                        *counts.entry(kw).or_insert(0) += 1;
                    }
                }
            }
        }

        let mut out: Vec<KeywordCount> = counts
            .into_iter()
            .map(|(keyword, count)| KeywordCount { keyword, count })
            .collect();
        out.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.keyword.cmp(&b.keyword))
        });
        out.truncate(limit);
        Ok(out)
    }

    /// Delete every analytics row.
    pub fn clear(&self) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM dictation_events", [])?;
        debug!("Cleared all dictation analytics events");
        Ok(())
    }
}

/// English stopwords excluded from keyword extraction (~100 common words).
const STOPWORDS: &[&str] = &[
    "the",
    "and",
    "for",
    "that",
    "this",
    "with",
    "you",
    "your",
    "are",
    "was",
    "were",
    "have",
    "has",
    "had",
    "not",
    "but",
    "all",
    "can",
    "her",
    "his",
    "him",
    "she",
    "they",
    "them",
    "their",
    "our",
    "out",
    "who",
    "get",
    "got",
    "how",
    "why",
    "what",
    "when",
    "where",
    "which",
    "would",
    "could",
    "should",
    "will",
    "just",
    "like",
    "into",
    "than",
    "then",
    "there",
    "here",
    "some",
    "such",
    "only",
    "over",
    "also",
    "back",
    "even",
    "very",
    "much",
    "more",
    "most",
    "other",
    "any",
    "each",
    "from",
    "about",
    "after",
    "before",
    "because",
    "been",
    "being",
    "does",
    "did",
    "doing",
    "done",
    "its",
    "itself",
    "him",
    "himself",
    "herself",
    "myself",
    "yourself",
    "ourselves",
    "themselves",
    "one",
    "two",
    "three",
    "off",
    "onto",
    "upon",
    "yes",
    "yeah",
    "okay",
    "well",
    "gonna",
    "wanna",
    "kind",
    "sort",
    "really",
    "actually",
    "basically",
    "literally",
    "maybe",
    "probably",
    "going",
    "want",
    "know",
    "think",
    "thing",
    "things",
    "make",
    "made",
    "need",
    "use",
    "used",
    "using",
    "let",
    "lets",
    "put",
    "way",
    "now",
    "new",
    "see",
    "say",
    "said",
    "and",
    "for",
    "are",
    "not",
    "with",
    "you",
    "the",
];

/// Lowercase, split on non-alphanumeric, drop short tokens and stopwords, count
/// frequencies, and return the top-N keywords by frequency.
pub fn extract_keywords(text: &str, top_n: usize) -> Vec<String> {
    let stop: std::collections::HashSet<&str> = STOPWORDS.iter().copied().collect();
    let mut counts: HashMap<String, i64> = HashMap::new();

    for token in text
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
    {
        if token.chars().count() < 3 {
            continue;
        }
        if stop.contains(token) {
            continue;
        }
        // Skip purely-numeric tokens (rarely meaningful as keywords).
        if token.chars().all(|c| c.is_numeric()) {
            continue;
        }
        *counts.entry(token.to_string()).or_insert(0) += 1;
    }

    let mut ranked: Vec<(String, i64)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.truncate(top_n);
    ranked.into_iter().map(|(k, _)| k).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_extraction_drops_stopwords_and_short_tokens() {
        let kws = extract_keywords(
            "The quick brown fox and the lazy dog jump over the fence quick fox",
            10,
        );
        // "quick" and "fox" appear twice, should rank first; stopwords excluded.
        assert!(kws.contains(&"quick".to_string()));
        assert!(kws.contains(&"fox".to_string()));
        assert!(!kws.contains(&"the".to_string()));
        assert!(!kws.contains(&"and".to_string()));
    }

    #[test]
    fn keyword_extraction_respects_top_n() {
        let kws = extract_keywords("alpha beta gamma delta epsilon zeta eta theta", 3);
        assert_eq!(kws.len(), 3);
    }
}
