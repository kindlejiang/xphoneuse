import { useState, useCallback, useEffect, useMemo, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useTranslation } from "react-i18next";
import type { Settings, Device, AndroidDevice, IosDevice, QuickActionId } from "./types";
import {
  PRESETS,
  DEFAULT_PINNED_ACTIONS,
  NON_STREAM_SETTING_KEYS,
  isIosDevice,
  resolveUnicodeTextMethod,
} from "./types";
import { setAppLanguage, type AppLanguage } from "./i18n";
import {
  SETTINGS_MUTATE_EVENT,
  SETTINGS_READY_EVENT,
  SETTINGS_RESULT_EVENT,
  SETTINGS_STATE_EVENT,
  type SettingsMutation,
  type SettingsSnapshot,
} from "./lib/settingsBridge";
import { useTheme } from "./hooks/useTheme";
import { useToasts } from "./hooks/useToasts";
import { useDevices } from "./hooks/useDevices";
import { useConnection } from "./hooks/useConnection";
import { useAdaptiveBitrate } from "./hooks/useAdaptiveBitrate";
import { useMacro } from "./hooks/useMacro";
import { useCopilot } from "./hooks/useCopilot";
import { useUpdater } from "./hooks/useUpdater";
import { WelcomeScreen } from "./components/WelcomeScreen";
import { MirrorScreen } from "./components/MirrorScreen";
import { CommandBar } from "./components/CommandBar";
import { MacrosScreen } from "./components/MacrosScreen";
import { ToastContainer } from "./components/ToastContainer";
import { IosDevModeDialog } from "./components/IosDevModeDialog";
import { UpdateDialog } from "./components/UpdateDialog";
import { resolveSaveDir, fileTimestamp, MOD } from "./lib/utils";
import "./App.css";

const isMac = navigator.userAgent.includes("Mac");

interface CommandDef {
  id: string;
  label: string;
  keys: string[];
  key: string;
  shift?: boolean;
  section: string;
  action: () => void;
}

function App() {
  const { t } = useTranslation();
  const [showCommandBar, setShowCommandBar] = useState(false);
  const [showMacros, setShowMacros] = useState(false);
  const [settings, setSettings] = useState<Settings>(() => {
    const stored = localStorage.getItem("stream_settings");
    if (stored) {
      try { return { ...PRESETS.balanced, ...JSON.parse(stored) }; } catch { }
    }
    return PRESETS.balanced;
  });
  const [activePreset, setActivePreset] = useState(() => {
    return localStorage.getItem("active_preset") || "balanced";
  });

  const [pinnedActions, setPinnedActions] = useState<QuickActionId[]>(() => {
    localStorage.removeItem("pinned_actions");
    return DEFAULT_PINNED_ACTIONS;
  });

  const [alwaysOnTop, setAlwaysOnTop] = useState(false);
  const toggleAlwaysOnTop = useCallback(async () => {
    const next = !alwaysOnTop;
    setAlwaysOnTop(next);
    await getCurrentWindow().setAlwaysOnTop(next);
  }, [alwaysOnTop]);

  const { theme, themePref, setThemePref, cycleTheme } = useTheme();
  const { toasts, showToast, updateToast, dismissToast } = useToasts();
  const { devices, refreshDevices, adbStatus, checkAdb } = useDevices(showToast);
  const macro = useMacro({ showToast, onRecordingStopped: () => setShowMacros(true) });
  const updater = useUpdater(showToast, updateToast, dismissToast);

  // Screenshot source for iOS: while AirPlay mirroring is live, the freshest
  // image is the decoded video frame on the canvas (a DVT capture can wedge
  // for tens of seconds on rich content). Filled in after useConnection
  // returns; read at click time.
  const screenshotSource = useRef<{
    canvasRef: React.RefObject<HTMLCanvasElement | null> | null;
    lastDrawAtRef: React.MutableRefObject<number> | null;
    lastDecodeAtRef: React.MutableRefObject<number> | null;
    paintLatestFrame: (() => boolean) | null;
    iosLive: boolean;
  }>({
    canvasRef: null,
    lastDrawAtRef: null,
    lastDecodeAtRef: null,
    paintLatestFrame: null,
    iosLive: false,
  });

  const takeScreenshot = useCallback(async () => {
    let base64: string;
    const { canvasRef, paintLatestFrame, iosLive } = screenshotSource.current;
    const liveCanvas = iosLive ? canvasRef?.current : null;
    if (liveCanvas) paintLatestFrame?.();
    if (liveCanvas && liveCanvas.width > 0) {
      base64 = liveCanvas.toDataURL("image/png").slice("data:image/png;base64,".length);
    } else {
      try {
        base64 = await invoke<string>("take_screenshot");
      } catch (e) {
        // A capture is often what discovers a missing developer image, and a
        // phone whose Developer Mode is off cannot install one - which is a
        // question for the user (the dialog), not an error message. The device
        // comes from the ref because this callback is not rebuilt per render.
        const device = connectedDeviceRef.current;
        if (!device || !askForDeveloperMode(device, e)) {
          showToast(t("toasts.screenshotFailed", { error: String(e) }));
        }
        return;
      }
    }
    // Copy the PNG to the system clipboard so it can be pasted right away.
    try {
      const bytes = Uint8Array.from(atob(base64), (c) => c.charCodeAt(0));
      await navigator.clipboard.write([
        new ClipboardItem({ "image/png": new Blob([bytes], { type: "image/png" }) }),
      ]);
      showToast(t("toasts.screenshotCopied"), "info");
    } catch (e) {
      showToast(t("toasts.screenshotCopyFailed", { error: String(e) }));
    }
    // Keep the configured save behavior: the Settings save location, else
    // the system Documents directory; browser download only as a fallback.
    try {
      const savePath = await resolveSaveDir();
      const filename = `screenshot-${fileTimestamp()}.png`;
      if (savePath) {
        const fullPath = `${savePath}/${filename}`;
        await invoke("save_file", { path: fullPath, data: base64 });
      } else {
        const link = document.createElement("a");
        link.href = `data:image/png;base64,${base64}`;
        link.download = filename;
        link.click();
      }
    } catch (e) {
      showToast(t("toasts.screenshotFailed", { error: String(e) }));
    }
  }, [showToast, t]);

  const adaptiveRef = useRef<{ frameReceived: () => void; disableAdaptive: () => void }>({
    frameReceived: () => { },
    disableAdaptive: () => { },
  });

  const settingsRef = useRef(settings);
  settingsRef.current = settings;
  const connectedDeviceRef = useRef<Device | null>(null);
  const scheduleReconnectRef = useRef<((s: Settings) => void) | null>(null);
  // Latest connect/devices for the MCP-driven connect listener below.
  const connectRef = useRef<(d: Device, s: Settings) => void>(() => { });
  const devicesRef = useRef<Device[]>([]);

  const handleCodecFallback = useCallback((codec: string) => {
    const next = { ...settingsRef.current, video_codec: codec };
    setSettings(next);
    if (connectedDeviceRef.current && !isIosDevice(connectedDeviceRef.current)) scheduleReconnectRef.current?.(next);
  }, []);

  const {
    screen,
    connectedDevice,
    connectingSerial,
    deviceSize,
    canvasRef,
    lastDrawAtRef,
    lastDecodeAtRef,
    paintLatestFrame,
    decoderRef,
    isMouseDownRef,
    muted,
    recording,
    streamStarted,
    iosHidAvailable,
    iosScreen,
    iosImagePrep,
    iosDevModePrompt,
    askForDeveloperMode,
    enableDeveloperMode,
    cancelDevModePrompt,
    continueDevModePrompt,
    setIosInputMode,
    agentInputReady,
    setMuted,
    toggleRecording,
    connectToDevice,
    disconnect,
    scheduleReconnect,
    pressButton,
    handleCanvasMouseEvent,
    handleWheel,
    handleKeyDown,
    handleCompositionStart,
    handleCompositionEnd,
    sendTextToDevice,
  } = useConnection({
    settings,
    showToast,
    takeScreenshot,
    setThemePref: (fn) => setThemePref(fn),
    onToggleAlwaysOnTop: toggleAlwaysOnTop,
    onFrameReceived: () => adaptiveRef.current.frameReceived(),
    onCodecFallback: handleCodecFallback,
    onRecordEvent: macro.recordEvent,
  });

  scheduleReconnectRef.current = scheduleReconnect;
  connectedDeviceRef.current = connectedDevice;
  screenshotSource.current.canvasRef = canvasRef;
  screenshotSource.current.lastDrawAtRef = lastDrawAtRef;
  screenshotSource.current.lastDecodeAtRef = lastDecodeAtRef;
  screenshotSource.current.paintLatestFrame = paintLatestFrame;
  screenshotSource.current.iosLive = !!connectedDevice && isIosDevice(connectedDevice) && streamStarted;
  connectRef.current = connectToDevice;
  devicesRef.current = devices;

  const adaptive = useAdaptiveBitrate({
    enabled: settings.adaptive,
    decoder: decoderRef,
    currentSettings: settings,
    onTierChange: (newSettings) => {
      setSettings(newSettings);
      setActivePreset("");
      if (connectedDevice && !isIosDevice(connectedDevice)) scheduleReconnect(newSettings);
    },
  });

  adaptiveRef.current = adaptive;

  // The copilot: watches the mirrored canvas for a pause, then reads the chat
  // off a screenshot and judges it. Off by default, and free when off (the
  // sampling timer only runs while it is on).
  const copilot = useCopilot({
    settings,
    connectedDevice,
    streamStarted,
    canvasRef,
    showToast,
  });

  // One sentence per stage of the developer-image work, with a percentage
  // while the download runs (see the line at the bottom of the render tree).
  const iosImagePrepLine = iosImagePrep
    ? (() => {
        if (iosImagePrep.stage === "mounting") return t("iosImage.mounting");
        if (iosImagePrep.stage !== "downloading") return t("iosImage.preparing");
        const { got = 0, total } = iosImagePrep;
        if (!total) return t("iosImage.preparing");
        return t("iosImage.downloading", { pct: Math.min(100, Math.floor((got / total) * 100)) });
      })()
    : "";

  /**
   * Settings live in their own OS window (see open_settings_window in
   * src-tauri/src/lib.rs): the mirror window is phone-shaped and the bottom
   * sheet it used to show covered most of it. Rust brings an already-open
   * window forward instead of creating a second one.
   */
  const openSettings = useCallback(() => {
    invoke("open_settings_window").catch((e) => showToast(String(e)));
  }, [showToast]);

  /**
   * The file manager is a third window of its own (open_file_manager_window in
   * src-tauri/src/lib.rs): it is a listing with a path bar and a drop target,
   * nothing a 380px-wide mirror can show. Same bring-forward rule as settings.
   */
  const openFileManager = useCallback(() => {
    invoke("open_file_manager_window").catch((e) => showToast(String(e)));
  }, [showToast]);

  const updateSetting = <K extends keyof Settings>(key: K, value: Settings[K]) => {
    // A stream parameter invalidates adaptive bitrate, the active preset and
    // the live Android session. The copilot's settings are none of those, and
    // treating them as stream parameters would make dragging the panel's
    // opacity slider drop the bitrate tier, clear the preset and reconnect the
    // phone - so the list of exceptions is shared rather than spelled out
    // twice (SettingsWindow.tsx mirrors this rule).
    const isStream = !NON_STREAM_SETTING_KEYS.includes(key);
    const next = { ...settings, [key]: value };
    if (isStream) {
      next.adaptive = false;
      adaptive.disableAdaptive();
    }
    setSettings(next);
    localStorage.setItem("stream_settings", JSON.stringify(next));
    if (isStream) {
      setActivePreset("");
      localStorage.setItem("active_preset", "");
    }
    if (isStream && connectedDevice && !isIosDevice(connectedDevice)) scheduleReconnect(next);
  };

  const applyPreset = (name: string) => {
    // The copilot's API keys are not part of a video preset; spreading the
    // preset over the settings would wipe them.
    const next = { ...PRESETS[name], audio: settings.audio, copilot: settings.copilot };
    setSettings(next);
    setActivePreset(name);
    localStorage.setItem("stream_settings", JSON.stringify(next));
    localStorage.setItem("active_preset", name);
    adaptive.disableAdaptive();
    if (connectedDevice && !isIosDevice(connectedDevice)) scheduleReconnect(next);
  };

  // ---- settings window bridge (see src/lib/settingsBridge.ts) --------------
  // The settings window renders a snapshot of the state below and sends its
  // edits back here. That keeps the reconnect scheduling, the adaptive-bitrate
  // shutdown and the live-session side effects in the window that owns the
  // session - they are the reason the settings cannot simply move across.
  const settingsSnapshot = useMemo<SettingsSnapshot>(() => ({
    settings,
    activePreset,
    pinnedActions,
    theme,
    viewOnly: connectedDevice ? isIosDevice(connectedDevice) : false,
    iosControllable: connectedDevice ? isIosDevice(connectedDevice) && iosHidAvailable : false,
    iosConnected: connectedDevice ? isIosDevice(connectedDevice) : false,
    iosVersion: connectedDevice && isIosDevice(connectedDevice) ? connectedDevice.ios_version : undefined,
    agentInputReady,
  }), [settings, activePreset, pinnedActions, theme, connectedDevice, iosHidAvailable, agentInputReady]);

  const snapshotRef = useRef(settingsSnapshot);
  snapshotRef.current = settingsSnapshot;
  const pushSnapshot = useCallback(() => { void emit(SETTINGS_STATE_EVENT, snapshotRef.current); }, []);

  // Every change is broadcast; while the settings window is closed, nobody
  // is listening.
  useEffect(() => { pushSnapshot(); }, [settingsSnapshot, pushSnapshot]);

  // The listeners below are registered once, so they reach the current
  // closures through refs.
  const updateSettingRef = useRef(updateSetting);
  updateSettingRef.current = updateSetting;
  const applyPresetRef = useRef(applyPreset);
  applyPresetRef.current = applyPreset;
  const setIosInputModeRef = useRef(setIosInputMode);
  setIosInputModeRef.current = setIosInputMode;

  useEffect(() => {
    const unlistenReady = listen(SETTINGS_READY_EVENT, () => pushSnapshot());
    const unlistenMutate = listen<SettingsMutation & { id?: number }>(SETTINGS_MUTATE_EVENT, (event) => {
      const m = event.payload;
      switch (m.kind) {
        case "updateSetting":
          updateSettingRef.current(m.key, m.value);
          break;
        case "applyPreset":
          applyPresetRef.current(m.name);
          break;
        case "pinnedActions":
          setPinnedActions(m.actions);
          break;
        case "iosInputMode":
          // Only this window can switch the live transport (it owns the
          // session refs), and the panel waits for the answer to put its
          // control back when the switch failed.
          void setIosInputModeRef.current(m.mode).then((ok) => {
            if (m.id !== undefined) void emit(SETTINGS_RESULT_EVENT, { id: m.id, ok });
          });
          break;
        case "language":
          setAppLanguage(m.lang as AppLanguage);
          break;
      }
    });
    return () => {
      unlistenReady.then((fn) => fn());
      unlistenMutate.then((fn) => fn());
    };
  }, [pushSnapshot]);

  const commands: CommandDef[] = useMemo(() => {
    const ios = connectedDevice ? isIosDevice(connectedDevice) : false;
    const iosControllable = ios && iosHidAvailable;
    const all: CommandDef[] = [
      { id: "vol-up", label: t("commands.labels.volUp"), keys: [MOD, "+"], key: "=", section: "audio", action: () => pressButton("volume_up") },
      { id: "vol-down", label: t("commands.labels.volDown"), keys: [MOD, "-"], key: "-", section: "audio", action: () => pressButton("volume_down") },
      { id: "mute", label: muted ? t("commands.labels.unmute") : t("commands.labels.mute"), keys: [MOD, "M"], key: "m", section: "audio", action: () => setMuted(!muted) },
      { id: "screenshot", label: t("commands.labels.screenshot"), keys: [MOD, "S"], key: "s", section: "actions", action: takeScreenshot },
      { id: "record", label: recording ? t("commands.labels.stopRecording") : t("commands.labels.recordScreen"), keys: [MOD, "⇧", "R"], key: "r", shift: true, section: "actions", action: toggleRecording },
      { id: "settings", label: t("commands.labels.openSettings"), keys: [MOD, ","], key: ",", section: "actions", action: openSettings },
      { id: "files", label: t("commands.labels.openFiles"), keys: [MOD, "⇧", "F"], key: "f", shift: true, section: "actions", action: openFileManager },
      { id: "theme", label: t("commands.labels.toggleTheme"), keys: [MOD, "T"], key: "t", section: "actions", action: cycleTheme },
      { id: "disconnect", label: t("commands.labels.disconnect"), keys: [MOD, "D"], key: "d", section: "actions", action: disconnect },
      { id: "home", label: t("commands.labels.home"), keys: [MOD, "H"], key: "h", section: "device", action: () => pressButton("home") },
      { id: "back", label: t("commands.labels.back"), keys: [MOD, "B"], key: "b", section: "device", action: () => pressButton("back") },
      { id: "recents", label: t("commands.labels.recents"), keys: [MOD, "R"], key: "r", section: "device", action: () => pressButton("recents") },
      { id: "power", label: t("commands.labels.powerButton"), keys: [MOD, "P"], key: "p", section: "device", action: () => pressButton("power") },
      { id: "rotate", label: t("commands.labels.rotateDevice"), keys: [MOD, "⇧", "O"], key: "o", shift: true, section: "device", action: () => { invoke("rotate_device").catch((e) => showToast(t("toasts.rotateFailed", { error: String(e) }))); } },
      { id: "cmdbar", label: t("commands.labels.commandBar"), keys: [MOD, "K"], key: "k", section: "actions", action: () => setShowCommandBar((s) => !s) },
      { id: "macro-toggle", label: macro.macroRecording ? t("commands.labels.stopMacroRecording") : t("commands.labels.recordMacro"), keys: [MOD, "⇧", "M"], key: "m", shift: true, section: "macros", action: macro.toggleRecording },
      { id: "macro-play", label: t("commands.labels.playLastMacro"), keys: [MOD, "⇧", "P"], key: "p", shift: true, section: "macros", action: () => { if (macro.macros.length > 0) macro.playMacro(macro.macros[macro.macros.length - 1].name); } },
      { id: "macro-manage", label: t("commands.labels.manageMacros"), keys: [MOD, "⇧", "L"], key: "l", shift: true, section: "macros", action: () => setShowMacros(true) },
      // Ends a looping macro; does nothing when none is running, which the
      // global handler needs (the key is bound whether or not a loop is on).
      { id: "macro-stop", label: t("commands.labels.stopMacro"), keys: [MOD, "E"], key: "e", section: "macros", action: macro.stopLoop },
      { id: "macro-export", label: t("commands.labels.exportAllMacros"), keys: [MOD, "⇧", "E"], key: "e", shift: true, section: "macros", action: macro.exportAllMacros },
      { id: "macro-import", label: t("commands.labels.importMacros"), keys: [MOD, "⇧", "I"], key: "i", shift: true, section: "macros", action: macro.importMacros },
      { id: "check-updates", label: t("commands.labels.checkUpdates"), keys: [MOD, "⇧", "U"], key: "u", shift: true, section: "actions", action: () => updater.checkForUpdates() },
      { id: "always-on-top", label: alwaysOnTop ? t("commands.labels.disableAlwaysOnTop") : t("commands.labels.alwaysOnTop"), keys: [MOD, "⇧", "T"], key: "t", shift: true, section: "actions", action: toggleAlwaysOnTop },
    ];
    // iOS without the HID bridge is view-only (no input/audio/macros).
    // With the bridge: power + volume work, home/back/recents/rotate/mute don't.
    if (!ios) return all;
    if (!iosControllable) {
      return all.filter((c) => c.section !== "device" && c.section !== "audio" && c.section !== "macros");
    }
    return all.filter((c) =>
      (c.section === "device" ? c.id === "power" : true) &&
      (c.section === "audio" ? c.id !== "mute" : true)
    );
  }, [t, connectedDevice, iosHidAvailable, muted, recording, setMuted, toggleRecording, pressButton, takeScreenshot, cycleTheme, disconnect, macro.macroRecording, macro.toggleRecording, macro.macros, macro.playMacro, macro.exportAllMacros, macro.importMacros, macro.stopLoop, updater.checkForUpdates, alwaysOnTop, toggleAlwaysOnTop, showToast, openSettings, openFileManager]);

  const commandsRef = useRef(commands);
  commandsRef.current = commands;

  useEffect(() => {
    // The MCP server shares this process and reads the value on every text
    // call, so hand it over before anything can be typed for us.
    invoke("set_unicode_text_default", { method: resolveUnicodeTextMethod() })
      .catch(() => { });
    const mcpEnabled = localStorage.getItem("mcp_enabled") !== "false";
    if (mcpEnabled) {
      const port = parseInt(localStorage.getItem("mcp_port") || "7070", 10);
      invoke("start_mcp_server", { port }).catch(() => { });
    }
    updater.checkForUpdates(true);
  }, []);

  // The phone leaving the device list (USB pulled, phone rebooted) ends the
  // session on this side too: otherwise the app sits on a dead tunnel, and iOS
  // keeps the screen-capture session - camera preview black - until something
  // stops it over a live connection. The backend remembers the interrupted
  // session and clears it when the device shows up again.
  const deviceSeenRef = useRef(false);
  useEffect(() => {
    if (!connectedDevice) {
      deviceSeenRef.current = false;
      return;
    }
    const key = isIosDevice(connectedDevice) ? connectedDevice.udid : connectedDevice.serial;
    // An iPhone can stay in the list over Wi-Fi sync after the cable is out,
    // but the tunnel runs on USB - so a USB session needs a USB entry. Any
    // other transport just needs the device to still be listed.
    const usbSession = isIosDevice(connectedDevice) && connectedDevice.connection_type === "usb";
    const present = devices.some((d) =>
      isIosDevice(connectedDevice)
        ? isIosDevice(d) && d.udid === key && (!usbSession || d.connection_type === "usb")
        : !isIosDevice(d) && d.serial === key
    );
    if (present) {
      deviceSeenRef.current = true;
      return;
    }
    // Never saw it in the list yet: the first refresh has not landed, or the
    // session was started from the MCP side - either way this is not a
    // disappearance.
    if (!deviceSeenRef.current) return;
    const timer = setTimeout(() => void disconnect(), 1500);
    return () => clearTimeout(timer);
  }, [devices, connectedDevice, disconnect]);

  // The in-app MCP server (e.g. driven by xphoneuse-cli) asks the app to
  // connect a device: run the normal connect flow so the window switches to
  // the mirror screen and streams the video itself.
  useEffect(() => {
    const unlisten = listen<{ platform: string; serial: string }>("mcp-connect-device", (event) => {
      const { platform, serial } = event.payload;
      const current = connectedDeviceRef.current;
      if (platform === "ios") {
        if (current && isIosDevice(current) && current.udid === serial) return;
        const known = devicesRef.current.find(
          (d): d is IosDevice => isIosDevice(d) && d.udid === serial
        );
        const device: IosDevice = known ?? {
          platform: "ios",
          udid: serial,
          name: serial,
          model: "",
          product_type: "",
          ios_version: "",
          connection_type: "usb",
          paired: true,
          available: true,
        };
        connectRef.current(device, settingsRef.current);
      } else if (platform === "android") {
        if (current && !isIosDevice(current) && current.serial === serial) return;
        const known = devicesRef.current.find(
          (d): d is AndroidDevice => !isIosDevice(d) && d.serial === serial
        );
        const device: AndroidDevice = known ?? {
          platform: "android",
          serial,
          model: serial,
          state: "device",
        };
        connectRef.current(device, settingsRef.current);
      }
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  // The in-app MCP server asks for a live mirror frame by event; reply with
  // the current decoded canvas (or null when iOS mirroring is not running,
  // which makes the server fall back to DVT capture).
  useEffect(() => {
    const unlisten = listen<number>("mcp-frame-request", (event) => {
      const { canvasRef, lastDrawAtRef, lastDecodeAtRef, paintLatestFrame, iosLive } =
        screenshotSource.current;
      const canvas = iosLive ? canvasRef?.current : null;
      // rAF is parked while the window is minimized or fully covered, so the
      // canvas on screen is whatever was last visible. Paint the newest
      // decoded frame synchronously: a screenshot must never come from a
      // frozen canvas while the stream itself is healthy.
      if (canvas) paintLatestFrame?.();
      let data: string | null = null;
      if (canvas && canvas.width > 0) {
        data = canvas.toDataURL("image/png").slice("data:image/png;base64,".length);
      }
      // What this reply actually read, for the caller's source line: a wrong
      // image has to say whether it came from a detached/duplicate canvas or
      // from a canvas that simply has not been painted for a while.
      let diag = "";
      if (canvas) {
        const rect = canvas.getBoundingClientRect();
        const shown =
          rect.width > 0 &&
          document.elementFromPoint(rect.left + rect.width / 2, rect.top + rect.height / 2) === canvas;
        const paintAge = lastDrawAtRef?.current
          ? Math.round(performance.now() - lastDrawAtRef.current)
          : -1;
        const decodeAge = lastDecodeAtRef?.current
          ? Math.round(performance.now() - lastDecodeAtRef.current)
          : -1;
        diag = `canvas ${canvas.width}x${canvas.height}, attached=${canvas.isConnected}, ` +
          `visible=${shown}, canvases=${document.querySelectorAll("canvas").length}, ` +
          `last paint ${paintAge}ms ago, last decode ${decodeAge}ms ago, ` +
          `preview=${!!document.querySelector('[data-role="mirror-preview"]')}`;
      }
      invoke("mcp_frame_reply", { id: event.payload, data, diag }).catch(() => { });
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      const mod = isMac ? e.metaKey : e.ctrlKey;
      if (!mod) return;
      if (showCommandBar && e.key !== "k") return;

      const cmd = commandsRef.current.find(
        (c) => c.key === e.key.toLowerCase() && !c.shift === !e.shiftKey
      );
      if (cmd) {
        e.preventDefault();
        cmd.action();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [showCommandBar]);

  return (
    <>
      {showMacros ? (
        <MacrosScreen
          macros={macro.macros}
          macrosDir={macro.macrosDir}
          playingMacro={macro.playingMacro}
          loopingMacro={macro.loopingMacro}
          onBack={() => setShowMacros(false)}
          onPlay={(name) => {
            setShowMacros(false);
            setTimeout(() => macro.playMacro(name), 500);
          }}
          onLoop={(name) => {
            // Same reason as a single play: the mirror has to be on screen
            // (and streaming) before the first tap goes in.
            setShowMacros(false);
            setTimeout(() => macro.startLoop(name), 500);
          }}
          onStopLoop={macro.stopLoop}
          onDelete={macro.deleteMacro}
          onRename={macro.renameMacro}
          onReorder={macro.reorderMacros}
          onExport={macro.exportMacro}
          onExportAll={macro.exportAllMacros}
          onImport={macro.importMacros}
          onSetDir={macro.setMacrosDir}
          showToast={showToast}
        />
      ) : screen === "welcome" ? (
        <WelcomeScreen
          devices={devices}
          connectingSerial={connectingSerial}
          themePref={themePref}
          onCycleTheme={cycleTheme}
          onOpenSettings={openSettings}
          onRefreshDevices={refreshDevices}
          onConnectDevice={(d) => connectToDevice(d, settings)}
          showToast={showToast}
          adbAvailable={adbStatus?.available ?? null}
          adbError={adbStatus?.error}
          onCheckAdb={checkAdb}
        />
      ) : connectedDevice ? (
        <MirrorScreen
          connectedDevice={connectedDevice}
          viewOnly={isIosDevice(connectedDevice) && !iosHidAvailable}
          textInputEnabled={!isIosDevice(connectedDevice) || iosHidAvailable || agentInputReady}
          streamStarted={streamStarted}
          waitingHint={
            isIosDevice(connectedDevice) && iosScreen !== "tunnel"
              ? t("mirror.waitingAirplay", { name: t("common.appName") })
              : undefined
          }
          deviceSize={deviceSize}
          canvasRef={canvasRef}
          isMouseDownRef={isMouseDownRef}
          recording={recording}
          muted={muted}
          macroRecording={macro.macroRecording}
          macroLooping={macro.loopingMacro}
          onStopMacroLoop={macro.stopLoop}
          pinnedActions={pinnedActions}
          alwaysOnTop={alwaysOnTop}
          adaptiveInfo={settings.adaptive ? { enabled: true, tierName: adaptive.metrics.tierName, fps: adaptive.metrics.fps } : undefined}
          onToggleRecording={toggleRecording}
          onToggleMacroRecording={macro.toggleRecording}
          onPressButton={pressButton}
          onTakeScreenshot={takeScreenshot}
          onOpenSettings={openSettings}
          onOpenFileManager={openFileManager}
          onOpenCommandBar={() => setShowCommandBar(true)}
          onDisconnect={disconnect}
          onRotate={() => { invoke("rotate_device").catch((e) => showToast(t("toasts.rotateFailed", { error: String(e) }))); }}
          onToggleAlwaysOnTop={toggleAlwaysOnTop}
          onToggleMute={() => setMuted(!muted)}
          onCanvasMouseEvent={handleCanvasMouseEvent}
          onWheel={handleWheel}
          onKeyDown={handleKeyDown}
          onCompositionStart={handleCompositionStart}
          onCompositionEnd={handleCompositionEnd}
          onSendText={sendTextToDevice}
          copilot={copilot}
        />
      ) : null}

      <CommandBar
        open={showCommandBar}
        onOpenChange={setShowCommandBar}
        commands={commands}
      />
      {/* The iPhone's developer image is prepared inside connect_ios_device
          (an iOS update invalidates the one on the device, and the tunnel
          needs it). It downloads ~16MB, so it gets a line rather than a
          spinner that looks stuck. Toasts sit above this at bottom-14. */}
      {iosImagePrep && (
        <div className="fixed bottom-4 left-1/2 -translate-x-1/2 z-100 px-[18px] py-2 rounded-lg bg-surface border border-border text-xs font-medium text-foreground backdrop-blur-2xl">
          {iosImagePrepLine}
        </div>
      )}
      {/* The "needs Developer Mode" question, asked before the mirror opens:
          the switch is what installs the image the screen and screenshots come
          from, and turning it on restarts the phone. */}
      <IosDevModeDialog
        prompt={iosDevModePrompt}
        devicePresent={devices.some((d) => isIosDevice(d) && d.udid === iosDevModePrompt?.device.udid)}
        onCancel={cancelDevModePrompt}
        onEnableNow={enableDeveloperMode}
        onContinue={continueDevModePrompt}
      />
      {/* A found update asks before it downloads: the download is large and, on
          Windows, installing it closes the app. */}
      <UpdateDialog
        prompt={updater.prompt}
        onLater={updater.dismissPrompt}
        onInstall={updater.installUpdate}
      />
      <ToastContainer toasts={toasts} />
    </>
  );
}


export default App;
