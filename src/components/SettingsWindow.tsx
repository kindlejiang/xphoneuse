import { useCallback, useEffect, useRef, useState } from "react";
import { emit, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useTranslation } from "react-i18next";
import i18n from "../i18n";
import { SettingsPanel } from "./SettingsPanel";
import { ToastContainer } from "./ToastContainer";
import { useToasts } from "../hooks/useToasts";
import { applyThemeClass, resolveStoredTheme } from "../hooks/useTheme";
import type { Settings, QuickActionId } from "../types";
import { DEFAULT_PINNED_ACTIONS, NON_STREAM_SETTING_KEYS, PRESETS } from "../types";
import {
  SETTINGS_MUTATE_EVENT,
  SETTINGS_READY_EVENT,
  SETTINGS_RESULT_EVENT,
  SETTINGS_STATE_EVENT,
  type MutationResult,
  type SettingsMutation,
  type SettingsSnapshot,
} from "../lib/settingsBridge";

/**
 * The settings window (a second OS window — see `open_settings_window` in
 * src-tauri/src/lib.rs and src/main.tsx, which mounts this instead of App).
 *
 * The main window owns the settings and the live session, so this window is a
 * controlled view of it: everything it renders comes from the `settings-state`
 * snapshot, and every edit goes back as a `settings-mutate` event. The panel
 * is the same component the main window used to show in a bottom sheet, so
 * the two can never drift apart.
 *
 * Local edits are applied optimistically (a slider has to follow the pointer,
 * not a round trip); the snapshot that comes back is the authority and
 * replaces them, which is also how a rejected edit (an iOS input switch the
 * device refused) snaps back to the truth.
 */
export function SettingsWindow() {
  const { t } = useTranslation();
  const { toasts, showToast } = useToasts();
  const [snapshot, setSnapshot] = useState<SettingsSnapshot>(seedSnapshot);
  // Mutations answered with a result, keyed by the id they went out with.
  const pending = useRef(new Map<number, (ok: boolean) => void>());
  const nextId = useRef(0);

  const mutate = useCallback((mutation: SettingsMutation) => {
    setSnapshot((s) => applyOptimistic(s, mutation));
    void emit(SETTINGS_MUTATE_EVENT, mutation);
  }, []);

  /** A mutation whose outcome the panel needs (it reverts the control on false). */
  const mutateAsync = useCallback(async (mutation: SettingsMutation): Promise<boolean> => {
    const id = ++nextId.current;
    const answer = new Promise<boolean>((resolve) => {
      pending.current.set(id, resolve);
      // Switching the input transport can take a while (it may have to start
      // a tunnel), but a switch that never answers must not leave the control
      // spinning: after this the optimistic value stands, and the main
      // window's own error toast is what reports a failure.
      window.setTimeout(() => {
        if (pending.current.delete(id)) resolve(true);
      }, 30000);
    });
    setSnapshot((s) => applyOptimistic(s, mutation));
    void emit(SETTINGS_MUTATE_EVENT, { ...mutation, id });
    return answer;
  }, []);

  useEffect(() => {
    const unlistenState = listen<SettingsSnapshot>(SETTINGS_STATE_EVENT, (event) => {
      setSnapshot(event.payload);
    });
    const unlistenResult = listen<MutationResult>(SETTINGS_RESULT_EVENT, (event) => {
      const { id, ok } = event.payload;
      pending.current.get(id)?.(ok);
      pending.current.delete(id);
    });
    // Ask the main window for the state; the seeded one is only what this
    // window could read from localStorage on its own.
    void emit(SETTINGS_READY_EVENT);
    return () => {
      unlistenState.then((fn) => fn());
      unlistenResult.then((fn) => fn());
    };
  }, []);

  // Theme: painted from localStorage first (no flash of the wrong theme),
  // then following the main window, which is the only writer.
  useEffect(() => {
    applyThemeClass(snapshot.theme);
  }, [snapshot.theme]);

  // Language is the one setting the panel changes on its own (it calls
  // setAppLanguage directly), so the change is picked up here and forwarded;
  // this window has already switched itself.
  useEffect(() => {
    const onLanguageChanged = (lang: string) => {
      void emit(SETTINGS_MUTATE_EVENT, { kind: "language", lang } satisfies SettingsMutation);
    };
    i18n.on("languageChanged", onLanguageChanged);
    return () => { i18n.off("languageChanged", onLanguageChanged); };
  }, []);

  useEffect(() => {
    getCurrentWindow().setTitle(t("common.settings")).catch(() => { });
  }, [t, i18n.language]);

  return (
    <>
      <SettingsPanel
        viewOnly={snapshot.viewOnly}
        iosControllable={snapshot.iosControllable}
        iosConnected={snapshot.iosConnected}
        iosVersion={snapshot.iosVersion}
        onIosInputModeChange={(mode) => mutateAsync({ kind: "iosInputMode", mode })}
        agentInputReady={snapshot.agentInputReady}
        showToast={showToast}
        settings={snapshot.settings}
        activePreset={snapshot.activePreset}
        pinnedActions={snapshot.pinnedActions}
        onApplyPreset={(name) => mutate({ kind: "applyPreset", name })}
        onUpdateSetting={(key, value) => mutate({ kind: "updateSetting", key, value })}
        onPinnedActionsChange={(actions) => mutate({ kind: "pinnedActions", actions })}
      />
      <ToastContainer toasts={toasts} />
    </>
  );
}

/**
 * What the window can show before the main window answers: the same
 * localStorage the main window writes on every change, so the first paint is
 * already right rather than empty. The session flags are the exception — they
 * live in the main window's memory and arrive with the snapshot.
 */
function seedSnapshot(): SettingsSnapshot {
  let settings: Settings = PRESETS.balanced;
  try {
    const stored = localStorage.getItem("stream_settings");
    if (stored) settings = { ...PRESETS.balanced, ...JSON.parse(stored) };
  } catch { /* a corrupt entry falls back to the balanced preset */ }
  let pinnedActions: QuickActionId[] = DEFAULT_PINNED_ACTIONS;
  try {
    const stored = localStorage.getItem("pinned_actions");
    if (stored) pinnedActions = JSON.parse(stored);
  } catch { /* same */ }
  return {
    settings,
    activePreset: localStorage.getItem("active_preset") || "balanced",
    pinnedActions,
    theme: resolveStoredTheme(),
    viewOnly: false,
    iosControllable: false,
    iosConnected: false,
    iosVersion: undefined,
    agentInputReady: false,
  };
}

/**
 * The panel's edit, applied locally so the control responds immediately.
 * Mirrors the rules in App.tsx's updateSetting/applyPreset (touching a stream
 * setting turns adaptive off and leaves the preset); the main window's
 * snapshot is what settles any disagreement.
 */
function applyOptimistic(s: SettingsSnapshot, m: SettingsMutation): SettingsSnapshot {
  switch (m.kind) {
    case "updateSetting": {
      const settings = { ...s.settings, [m.key]: m.value } as Settings;
      if (NON_STREAM_SETTING_KEYS.includes(m.key)) return { ...s, settings };
      settings.adaptive = false;
      return { ...s, settings, activePreset: "" };
    }
    case "applyPreset":
      return {
        ...s,
        settings: { ...PRESETS[m.name], audio: s.settings.audio, copilot: s.settings.copilot },
        activePreset: m.name,
      };
    case "pinnedActions":
      return { ...s, pinnedActions: m.actions };
    default:
      return s;
  }
}
