//! Pinned session-slot store for CLI-agent session hotkeys.
//!
//! Pure bookkeeping over up to 9 **stable** slots per agent, persisted as JSON
//! (`agent_sessions.json`, sibling of the settings store). Slots never renumber:
//! slot 1 stays slot 1 across uses. A new session takes the lowest free slot, or
//! evicts the least-recently-used slot when all 9 are full. See
//! `documentation/design/session-hotkeys/DESIGN.md`.
//!
//! Timestamps are stored as RFC3339 strings (specta `Type`-friendly and the exact
//! shape in the DESIGN's JSON); `assign`/`touch` take a `DateTime<Utc>` so callers
//! inject a clock (fixed timestamps in tests, `Utc::now()` in production).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use specta::Type;

/// Maximum pinned slots per agent.
pub const MAX_SLOTS: u8 = 9;

/// Tauri-managed holder: the loaded store plus where it persists. Loaded once at
/// startup; guarded by a `Mutex` because runs (streaming tasks) and commands both
/// mutate it.
pub struct SessionSlotState {
    pub store: Mutex<SlotStore>,
    pub path: PathBuf,
}

impl SessionSlotState {
    /// Load the store from `path` (empty if missing/corrupt) and wrap it.
    pub fn new(path: PathBuf) -> Self {
        SessionSlotState {
            store: Mutex::new(SlotStore::load(&path)),
            path,
        }
    }
}

/// One pinned, resumable CLI-agent session.
#[derive(Clone, Debug, Serialize, Deserialize, Type, PartialEq)]
pub struct SessionSlot {
    /// Stable slot number, 1..=9. Never renumbered.
    pub slot: u8,
    /// The CLI's session/thread id used to resume.
    pub session_id: String,
    /// Working directory the session was created in (resume must match it).
    pub project_path: String,
    /// Human label — first ~48 chars of the first instruction.
    pub label: String,
    /// RFC3339 UTC creation time.
    pub created_at: String,
    /// RFC3339 UTC last-used time (updated on resume; drives LRU eviction).
    pub last_used_at: String,
}

/// Persisted per-agent slot store.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SlotStore {
    pub version: u32,
    /// agent_id -> its slots (unordered; slot numbers are authoritative).
    pub agents: HashMap<String, Vec<SessionSlot>>,
}

impl Default for SlotStore {
    fn default() -> Self {
        SlotStore {
            version: 1,
            agents: HashMap::new(),
        }
    }
}

impl SlotStore {
    /// Load from `path`. Missing file → empty store. Unparseable file → the file
    /// is renamed to `<path>.bak` and an empty store is returned (never blocks a run).
    pub fn load(path: &Path) -> SlotStore {
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => return SlotStore::default(),
        };
        match serde_json::from_str::<SlotStore>(&raw) {
            Ok(store) => store,
            Err(_) => {
                // Corrupt: preserve for forensics, start clean.
                let bak = path.with_extension("json.bak");
                let _ = std::fs::rename(path, &bak);
                SlotStore::default()
            }
        }
    }

    /// Persist atomically (write to `.tmp`, then rename over `path`).
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Slots for an agent, sorted by slot number (1..=9). Empty if none.
    pub fn slots_for(&self, agent_id: &str) -> Vec<SessionSlot> {
        let mut v = self.agents.get(agent_id).cloned().unwrap_or_default();
        v.sort_by_key(|s| s.slot);
        v
    }

    /// Assign a new session to the lowest free slot, or evict the LRU slot when
    /// all 9 are full (reusing its number). Returns the assigned slot number.
    pub fn assign(
        &mut self,
        agent_id: &str,
        session_id: &str,
        project_path: &str,
        label: &str,
        now: DateTime<Utc>,
    ) -> u8 {
        let ts = now.to_rfc3339();
        let entries = self.agents.entry(agent_id.to_string()).or_default();

        let slot = if let Some(free) = lowest_free_slot(entries) {
            free
        } else {
            // All full: evict the entry with the oldest last_used_at, reuse its slot.
            let evict_idx = lru_index(entries);
            let reused = entries[evict_idx].slot;
            entries.remove(evict_idx);
            reused
        };

        entries.push(SessionSlot {
            slot,
            session_id: session_id.to_string(),
            project_path: project_path.to_string(),
            label: label.to_string(),
            created_at: ts.clone(),
            last_used_at: ts,
        });
        slot
    }

    /// Resolve a slot number to its entry, if occupied.
    pub fn resolve(&self, agent_id: &str, slot: u8) -> Option<&SessionSlot> {
        self.agents
            .get(agent_id)?
            .iter()
            .find(|s| s.slot == slot)
    }

    /// Mark the slot holding `session_id` used now (resume path). Returns the slot
    /// number if found. Updates `last_used_at` only.
    pub fn touch_by_session_id(
        &mut self,
        agent_id: &str,
        session_id: &str,
        now: DateTime<Utc>,
    ) -> Option<u8> {
        let entries = self.agents.get_mut(agent_id)?;
        let e = entries.iter_mut().find(|s| s.session_id == session_id)?;
        e.last_used_at = now.to_rfc3339();
        Some(e.slot)
    }

    /// Mark a slot used now (resume). Updates `last_used_at` only.
    pub fn touch(&mut self, agent_id: &str, slot: u8, now: DateTime<Utc>) {
        if let Some(entries) = self.agents.get_mut(agent_id) {
            if let Some(e) = entries.iter_mut().find(|s| s.slot == slot) {
                e.last_used_at = now.to_rfc3339();
            }
        }
    }

    /// Remove a slot, freeing its number for reuse.
    pub fn clear(&mut self, agent_id: &str, slot: u8) {
        if let Some(entries) = self.agents.get_mut(agent_id) {
            entries.retain(|s| s.slot != slot);
        }
    }
}

/// Lowest unoccupied slot number in 1..=9, or None if all taken.
fn lowest_free_slot(entries: &[SessionSlot]) -> Option<u8> {
    (1..=MAX_SLOTS).find(|n| !entries.iter().any(|s| s.slot == *n))
}

/// Index of the least-recently-used entry (oldest `last_used_at`). Assumes
/// non-empty. Unparseable timestamps sort oldest (evicted first).
fn lru_index(entries: &[SessionSlot]) -> usize {
    entries
        .iter()
        .enumerate()
        .min_by_key(|(_, s)| {
            DateTime::parse_from_rfc3339(&s.last_used_at)
                .map(|d| d.timestamp_millis())
                .unwrap_or(i64::MIN)
        })
        .map(|(i, _)| i)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(min: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 27, 10, 0, 0).unwrap() + chrono::Duration::minutes(min)
    }

    #[test]
    fn assign_uses_lowest_free_slot_and_is_stable() {
        let mut s = SlotStore::default();
        assert_eq!(s.assign("coder", "sid-a", "/p", "first", t(0)), 1);
        assert_eq!(s.assign("coder", "sid-b", "/p", "second", t(0)), 2);
        s.clear("coder", 1);
        assert_eq!(s.assign("coder", "sid-c", "/p", "third", t(0)), 1); // reuses freed slot
        assert_eq!(s.resolve("coder", 2).unwrap().session_id, "sid-b"); // 2 untouched
    }

    #[test]
    fn tenth_assign_evicts_lru_slot() {
        let mut s = SlotStore::default();
        for i in 0..9 {
            s.assign("coder", &format!("sid-{i}"), "/p", "x", t(i));
        }
        s.touch("coder", 1, t(120)); // slot 1 (sid-0) now most recent
        // LRU is slot 2 (sid-1, minute 1)
        assert_eq!(s.assign("coder", "sid-new", "/p", "new", t(180)), 2);
        assert_eq!(s.resolve("coder", 2).unwrap().session_id, "sid-new");
        // slot 1 survived (it was touched)
        assert_eq!(s.resolve("coder", 1).unwrap().session_id, "sid-0");
    }

    #[test]
    fn touch_updates_last_used_only() {
        let mut s = SlotStore::default();
        s.assign("coder", "sid-a", "/p", "first", t(0));
        let created = s.resolve("coder", 1).unwrap().created_at.clone();
        s.touch("coder", 1, t(30));
        let e = s.resolve("coder", 1).unwrap();
        assert_eq!(e.created_at, created); // unchanged
        assert_eq!(e.last_used_at, t(30).to_rfc3339()); // moved
        assert_eq!(e.session_id, "sid-a");
    }

    #[test]
    fn resolve_empty_agent_is_none() {
        let s = SlotStore::default();
        assert!(s.resolve("nobody", 1).is_none());
    }

    #[test]
    fn slots_for_returns_sorted() {
        let mut s = SlotStore::default();
        s.assign("a", "s1", "/p", "l1", t(0));
        s.assign("a", "s2", "/p", "l2", t(1));
        s.clear("a", 1);
        s.assign("a", "s3", "/p", "l3", t(2)); // takes slot 1 again
        let slots = s.slots_for("a");
        assert_eq!(slots.iter().map(|x| x.slot).collect::<Vec<_>>(), vec![1, 2]);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = std::env::temp_dir().join(format!("ofs-slots-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("agent_sessions.json");
        let mut s = SlotStore::default();
        s.assign("coder", "sid-a", "/proj", "a label", t(0));
        s.save(&path).unwrap();
        let loaded = SlotStore::load(&path);
        assert_eq!(loaded.resolve("coder", 1).unwrap().session_id, "sid-a");
        assert_eq!(loaded.version, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_file_is_empty_store() {
        let path = std::env::temp_dir().join("ofs-slots-does-not-exist-xyz.json");
        let _ = std::fs::remove_file(&path);
        let s = SlotStore::load(&path);
        assert!(s.agents.is_empty());
        assert_eq!(s.version, 1);
    }

    #[test]
    fn load_corrupt_file_backs_up_and_returns_empty() {
        let dir = std::env::temp_dir().join(format!("ofs-slots-corrupt-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("agent_sessions.json");
        std::fs::write(&path, "{ this is not valid json ]").unwrap();
        let s = SlotStore::load(&path);
        assert!(s.agents.is_empty());
        // Corrupt original renamed to .bak, original gone.
        assert!(path.with_extension("json.bak").exists());
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
