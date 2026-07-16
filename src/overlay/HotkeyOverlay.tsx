import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import React, { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import "./HotkeyOverlay.css";
import { commands } from "@/bindings";
import type { AppSettings } from "@/bindings";
import i18n, { syncLanguageFromSettings } from "@/i18n";
import { getLanguageDirection } from "@/lib/utils/rtl";
import { formatKeyCombination } from "@/lib/utils/keyboard";

interface HotkeyRow {
  id: string;
  name: string;
  combo: string[];
}

interface HotkeyGroup {
  key: string;
  title: string;
  rows: HotkeyRow[];
}

// Split a raw binding string ("option+shift+space") into display chips, reusing
// the shared formatter so the labels match the rest of the app exactly.
function comboChips(raw: string): string[] {
  const formatted = formatKeyCombination(raw, "macos");
  return formatted ? formatted.split(" + ") : [];
}

/**
 * Phase D2: the hotkey cheat-sheet overlay. Shown (HOLD) via the `hotkey_overlay`
 * binding — the backend emits `show-hotkey-overlay` on press and
 * `hide-hotkey-overlay` on release. On show it reads the current settings and
 * renders every binding with a non-empty hotkey, grouped Dictation / AI Modes /
 * Agents / Meetings. Esc / blur / a 30s failsafe dismiss it defensively.
 */
const HotkeyOverlay: React.FC = () => {
  const { t } = useTranslation();
  const [isVisible, setIsVisible] = useState(false);
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const direction = getLanguageDirection(i18n.language);

  useEffect(() => {
    let failsafe: ReturnType<typeof setTimeout> | undefined;

    const hideSelf = () => {
      setIsVisible(false);
      // Defensively hide the native window too (the backend also hides it on
      // release; this covers Esc / blur / the failsafe).
      void getCurrentWindow().hide();
    };

    const setup = async () => {
      const unlistenShow = await listen("show-hotkey-overlay", async () => {
        await syncLanguageFromSettings();
        try {
          const res = await commands.getAppSettings();
          if (res.status === "ok") setSettings(res.data);
        } catch {
          // Keep the last-known settings if the read fails.
        }
        setIsVisible(true);
        if (failsafe) clearTimeout(failsafe);
        failsafe = setTimeout(hideSelf, 30_000);
      });

      const unlistenHide = await listen("hide-hotkey-overlay", () => {
        setIsVisible(false);
        if (failsafe) clearTimeout(failsafe);
      });

      return () => {
        unlistenShow();
        unlistenHide();
      };
    };

    const cleanupPromise = setup();

    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") hideSelf();
    };
    const onBlur = () => hideSelf();
    const onVisibility = () => {
      if (document.visibilityState === "hidden") setIsVisible(false);
    };
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("blur", onBlur);
    document.addEventListener("visibilitychange", onVisibility);

    return () => {
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("blur", onBlur);
      document.removeEventListener("visibilitychange", onVisibility);
      if (failsafe) clearTimeout(failsafe);
      void cleanupPromise.then((fn) => fn && fn());
    };
  }, []);

  const groups = useMemo<HotkeyGroup[]>(() => {
    if (!settings) return [];
    const bindings = settings.bindings ?? {};
    const agents = settings.agents ?? [];
    const modes = settings.ai_modes ?? [];

    const bound = (id: string): string | null => {
      const b = bindings[id];
      const cur = b?.current_binding?.trim();
      return cur ? cur : null;
    };

    const row = (id: string, name: string): HotkeyRow | null => {
      const cur = bound(id);
      if (!cur) return null;
      return { id, name, combo: comboChips(cur) };
    };

    // Dictation: transcribe, post-process (legacy), cancel, and the cheat-sheet
    // hotkey itself.
    const dictation: HotkeyRow[] = [
      row("transcribe", t("settings.hotkeyOverlay.bindings.transcribe")),
      row(
        "transcribe_with_post_process",
        t("settings.hotkeyOverlay.bindings.postProcess"),
      ),
      row("cancel", t("settings.hotkeyOverlay.bindings.cancel")),
      row("hotkey_overlay", t("settings.hotkeyOverlay.bindings.showHotkeys")),
    ].filter((r): r is HotkeyRow => r !== null);

    // AI Modes: user-given names, from the `mode:<id>` bindings.
    const modeRows: HotkeyRow[] = modes
      .map((m) => row(`mode:${m.id}`, m.name))
      .filter((r): r is HotkeyRow => r !== null);

    // Agents: user-given names, from the `agent:<id>` bindings.
    const agentRows: HotkeyRow[] = agents
      .map((a) => row(`agent:${a.id}`, a.name))
      .filter((r): r is HotkeyRow => r !== null);

    // Meetings.
    const meetingRows: HotkeyRow[] = [
      row(
        "meeting_capture",
        t("settings.hotkeyOverlay.bindings.meetingCapture"),
      ),
    ].filter((r): r is HotkeyRow => r !== null);

    const all: HotkeyGroup[] = [
      {
        key: "dictation",
        title: t("settings.hotkeyOverlay.groups.dictation"),
        rows: dictation,
      },
      {
        key: "aiModes",
        title: t("settings.hotkeyOverlay.groups.aiModes"),
        rows: modeRows,
      },
      {
        key: "agents",
        title: t("settings.hotkeyOverlay.groups.agents"),
        rows: agentRows,
      },
      {
        key: "meetings",
        title: t("settings.hotkeyOverlay.groups.meetings"),
        rows: meetingRows,
      },
    ];
    return all.filter((g) => g.rows.length > 0);
  }, [settings, t]);

  const hasAny = groups.length > 0;

  return (
    <div
      dir={direction}
      className={`hk-stage ${isVisible ? "show" : ""}`}
      aria-hidden={!isVisible}
    >
      <div className="hk-card">
        <div className="hk-header">
          <span className="hk-title">{t("settings.hotkeyOverlay.title")}</span>
        </div>
        <div className="hk-body">
          {hasAny ? (
            groups.map((group) => (
              <div key={group.key} className="hk-group">
                <div className="hk-group-title">{group.title}</div>
                {group.rows.map((r) => (
                  <div key={r.id} className="hk-row">
                    <span className="hk-name">{r.name}</span>
                    <span className="hk-combo">
                      {r.combo.map((chip, i) => (
                        <kbd key={i} className="hk-key">
                          {chip}
                        </kbd>
                      ))}
                    </span>
                  </div>
                ))}
              </div>
            ))
          ) : (
            <div className="hk-empty">{t("settings.hotkeyOverlay.empty")}</div>
          )}
        </div>
      </div>
    </div>
  );
};

export default HotkeyOverlay;
