import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import "./SessionPicker.css";
import { commands } from "@/bindings";
import type { SessionSlot } from "@/bindings";

/** Last path segment (project folder name) from an absolute path. */
function basename(p: string): string {
  if (!p) return "";
  const parts = p.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? p;
}

/** Compact relative age from an RFC3339 timestamp. */
function relAge(iso: string): string {
  const then = new Date(iso).getTime();
  if (Number.isNaN(then)) return "";
  const secs = Math.max(0, Math.floor((Date.now() - then) / 1000));
  if (secs < 60) return `${secs}s ago`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  return `${Math.floor(hrs / 24)}d ago`;
}

/**
 * Session hotkeys: the non-focusable picker overlay. Shown while an agent
 * recording is live (backend emits `session-overlay-show` with the agent id).
 * It fetches that agent's pinned slots, highlights the digit the user pressed
 * (`session-slot-selected`), and hides on `session-overlay-hide`. Selection and
 * dispatch happen in Rust — this is display only.
 */
const SessionPicker: React.FC = () => {
  const { t } = useTranslation();
  const [isVisible, setVisible] = useState(false);
  const [slots, setSlots] = useState<SessionSlot[]>([]);
  const [selected, setSelected] = useState<number | null>(null);

  useEffect(() => {
    const unlisteners: Array<() => void> = [];

    const setup = async () => {
      unlisteners.push(
        await listen<string>("session-overlay-show", async (e) => {
          setSelected(null);
          try {
            const res = await commands.getSessionSlots(e.payload);
            setSlots(res.status === "ok" ? res.data : []);
          } catch {
            setSlots([]);
          }
          setVisible(true);
        }),
      );
      unlisteners.push(
        await listen<{ selection: number | null }>(
          "session-slot-selected",
          (e) => setSelected(e.payload.selection),
        ),
      );
      unlisteners.push(
        await listen("session-overlay-hide", () => {
          setVisible(false);
          void getCurrentWindow().hide();
        }),
      );
    };

    void setup();
    return () => unlisteners.forEach((fn) => fn());
  }, []);

  return (
    <div
      className={`sp-stage ${isVisible ? "show" : ""}`}
      aria-hidden={!isVisible}
    >
      <div className="sp-card">
        <div className="sp-header">
          <span className="sp-title">{t("settings.sessionPicker.title")}</span>
          <span className="sp-hint">{t("settings.sessionPicker.hint")}</span>
        </div>
        <div className="sp-body">
          {slots.length === 0 ? (
            <div className="sp-empty">{t("settings.sessionPicker.empty")}</div>
          ) : (
            slots.map((s) => (
              <div
                key={s.slot}
                className={`sp-row ${selected === s.slot ? "sel" : ""}`}
              >
                <kbd className="sp-key">{s.slot}</kbd>
                <span className="sp-proj">{basename(s.project_path)}</span>
                <span className="sp-label">{s.label}</span>
                <span className="sp-age">{relAge(s.last_used_at)}</span>
              </div>
            ))
          )}
          <div className={`sp-row sp-new ${selected === 0 ? "sel" : ""}`}>
            <kbd className="sp-key">0</kbd>
            <span className="sp-label">
              {t("settings.sessionPicker.newSession")}
            </span>
          </div>
        </div>
      </div>
    </div>
  );
};

export default SessionPicker;
