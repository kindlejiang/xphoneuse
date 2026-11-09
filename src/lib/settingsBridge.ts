import type { Settings, QuickActionId } from "../types";

/**
 * The settings window is a second OS window (see `open_settings_window` in
 * src-tauri/src/lib.rs), so the two windows have to agree on one copy of the
 * settings. The main window stays the only writer: it owns the React state
 * and, more importantly, the live session — `updateSetting` schedules the
 * Android reconnect, `applyPreset` turns adaptive bitrate off, and the iOS
 * input switch pokes refs inside useConnection. None of that can move to
 * another window without splitting the session in two.
 *
 * So the settings window is a controlled view: the main window pushes a
 * snapshot of everything the panel needs, and the panel sends the user's
 * intent back. Both directions are plain Tauri events between the two
 * webviews — Rust is not involved.
 */

/** Main window -> settings window: the full state the panel renders from. */
export const SETTINGS_STATE_EVENT = "settings-state";
/** Settings window -> main window: "I am up, send me the state". */
export const SETTINGS_READY_EVENT = "settings-window-ready";
/** Settings window -> main window: the user changed something. */
export const SETTINGS_MUTATE_EVENT = "settings-mutate";
/** Main window -> settings window: the result of a mutation that has one. */
export const SETTINGS_RESULT_EVENT = "settings-mutate-result";

export interface SettingsSnapshot {
  settings: Settings;
  activePreset: string;
  pinnedActions: QuickActionId[];
  /** Resolved theme ("auto" already collapsed to light/dark). */
  theme: "light" | "dark";
  /** iOS session without the HID bridge: hide everything but the recorder. */
  viewOnly: boolean;
  /** iOS session with the bridge: only volume/power quick actions. */
  iosControllable: boolean;
  /** An iOS device is connected (USB forward for AgentInput available). */
  iosConnected: boolean;
  /** iOS version of the connected device (gates the tunnel options). */
  iosVersion?: string;
  /** An AgentInput keyboard client is configured and reachable. */
  agentInputReady: boolean;
}

export type SettingsMutation =
  | { kind: "updateSetting"; key: keyof Settings; value: Settings[keyof Settings] }
  | { kind: "applyPreset"; name: string }
  | { kind: "pinnedActions"; actions: QuickActionId[] }
  | { kind: "iosInputMode"; mode: "bridge" | "tunnel" }
  | { kind: "language"; lang: string };

/** A mutation carrying an id is answered with `settings-mutate-result`. */
export type IdentifiedMutation = SettingsMutation & { id: number };

export interface MutationResult {
  id: number;
  ok: boolean;
}
