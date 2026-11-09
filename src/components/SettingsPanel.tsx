import { useState, useEffect /*, useCallback (used by the commented-out MCP block) */ } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { useTranslation } from "react-i18next";
import {
  ArrowTopRightOnSquareIcon,
  ChevronDownIcon,
//   ClipboardDocumentIcon,
  FolderOpenIcon,
} from "@heroicons/react/24/outline";
import {
  Collapsible,
  CollapsibleTrigger,
  CollapsibleContent,
} from "@/components/ui/collapsible";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Button } from "@/components/ui/button";
import { Slider } from "@/components/ui/slider";
import { Switch } from "@/components/ui/switch";
import { CopilotSettings } from "./CopilotSettings";
import { LocationSettings } from "./LocationSettings";
import { APP_VERSION, cn, resolveSaveDir, openHomepage } from "@/lib/utils";
import type { Settings, QuickActionId } from "../types";
import { PRESETS, RESOLUTION_OPTIONS, CODEC_OPTIONS, QUICK_ACTIONS, iosSupportsTunnel, resolveIosScreenMode, resolveIosInputMode, resolveUnicodeTextMethod, setUnicodeTextMethod } from "../types";
import { APP_LANGUAGES, setAppLanguage } from "../i18n";

// const DEFAULT_MCP_PORT = 7070; // used by the commented-out MCP block

// function getMcpUrl(port: number) {
//   return `http://localhost:${port}/mcp`;
// }

// function getMcpConfig(port: number) {
//   return JSON.stringify({
//     mcpServers: {
//       xphoneuse: { type: "http", url: getMcpUrl(port) },
//     },
//   }, null, 2);
// }

interface SettingsPanelProps {
  /** Hide Android-only stream settings (presets/video/audio) for iOS sessions. */
  viewOnly?: boolean;
  /** iOS session with the HID bridge: allow only volume/power quick actions. */
  iosControllable?: boolean;
  /** An iOS device is currently connected (USB forward for AgentInput available). */
  iosConnected?: boolean;
  /** iOS version of the connected device (gates the tunnel options). */
  iosVersion?: string;
  /** Switch the live iOS input transport (see useConnection::setIosInputMode). */
  onIosInputModeChange?: (mode: "bridge" | "tunnel") => Promise<boolean>;
  /** A AgentInput (iOS text-input keyboard) client is configured and reachable. */
  agentInputReady?: boolean;
  onAgentInputChange?: (ready: boolean) => void;
  showToast?: (msg: string, type?: "error" | "info") => void;
  settings: Settings;
  activePreset: string;
  pinnedActions: QuickActionId[];
  onApplyPreset: (name: string) => void;
  onUpdateSetting: <K extends keyof Settings>(key: K, value: Settings[K]) => void;
  onPinnedActionsChange: (actions: QuickActionId[]) => void;
}
//
// function copyToClipboard(text: string) {
//   navigator.clipboard.writeText(text);
// }

function SectionHeader({ children, open }: { children: React.ReactNode; open?: boolean }) {
  return (
    <CollapsibleTrigger className="flex items-center justify-between w-full px-5 py-3 border-b border-border/60 cursor-pointer hover:bg-surface-hover/50 transition-colors">
      <span className="text-[10px] font-bold uppercase tracking-widest text-text-3">{children}</span>
      <ChevronDownIcon className={cn("size-3 text-text-3 transition-transform duration-200", open && "rotate-180")} />
    </CollapsibleTrigger>
  );
}

export function SettingsPanel({
  viewOnly,
  iosControllable,
  iosConnected,
  iosVersion,
  onIosInputModeChange,
//   agentInputReady,
//   onAgentInputChange,
  showToast,
  settings,
  activePreset,
  pinnedActions,
  onApplyPreset,
  onUpdateSetting,
  onPinnedActionsChange,
}: SettingsPanelProps) {
  const { t, i18n } = useTranslation();
//   const [mcpInstructionsOpen, setMcpInstructionsOpen] = useState(false);
//   const [copiedUrl, setCopiedUrl] = useState(false);
//   const [copiedSnippet, setCopiedSnippet] = useState<string | null>(null);

  const [videoOpen, setVideoOpen] = useState(true);
  const [audioOpen, setAudioOpen] = useState(true);
  const [storageOpen, setStorageOpen] = useState(true);
//   const [mcpOpen, setMcpOpen] = useState(true);
  const [savePath, setSavePath] = useState(() => localStorage.getItem("save_path") || "");
  // Effective default when no save location is configured: the system
  // Documents directory (screenshots and recordings land there).
  const [defaultSaveDir, setDefaultSaveDir] = useState<string | null>(null);
  useEffect(() => {
    if (localStorage.getItem("save_path")) return;
    resolveSaveDir().then(setDefaultSaveDir).catch(() => { });
  }, [savePath]);
  const [toolbarOpen, setToolbarOpen] = useState(true);

//   const [mcpEnabled, setMcpEnabled] = useState(() => {
//     const stored = localStorage.getItem("mcp_enabled");
//     return stored === null ? true : stored === "true";
//   });
//   const [mcpPort] = useState(() => {
//     const stored = localStorage.getItem("mcp_port");
//     return stored ? parseInt(stored, 10) : DEFAULT_MCP_PORT;
//   });
//   const [mcpRunning, setMcpRunning] = useState(false);

  // iOS transports (Settings → "iOS Connection"): where the live screen and
  // the input come from. The CoreDevice USB tunnel needs iOS >= 27 and is the
  // default there; below that (and originally) it is AirPlay + BLE-HID bridge.
  const [iosTransportOpen, setIosTransportOpen] = useState(false);
  // Collapsed by default: the map is a big block, and most sessions never
  // touch it.
  const [locationOpen, setLocationOpen] = useState(false);
  const [iosScreenMode, setIosScreenMode] = useState<"airplay" | "tunnel">(
    () => resolveIosScreenMode(),
  );
  const [iosInputMode, setIosInputMode] = useState<"bridge" | "tunnel">(
    () => resolveIosInputMode(),
  );
  // How non-ASCII text reaches an iPhone: the device clipboard (+ Cmd+V) or
  // the AgentInput keyboard. Rust needs this — the MCP server asks for it per
  // call — so it is pushed on mount (App.tsx) and again on every change.
  const [unicodeMethod, setUnicodeMethod] = useState<"clipboard" | "agentinput">(
    () => resolveUnicodeTextMethod(),
  );
  const tunnelBlocked = iosVersion !== undefined && !iosSupportsTunnel(iosVersion);
  // The selectors must mirror what connectToDevice will actually use, so
  // re-resolve the defaults whenever the connected device changes.
  useEffect(() => {
    setIosScreenMode(resolveIosScreenMode(iosVersion));
    setIosInputMode(resolveIosInputMode(iosVersion));
  }, [iosVersion]);

  function handleIosScreenMode(mode: string | null) {
    if (!mode) return;
    setIosScreenMode(mode as "airplay" | "tunnel");
    localStorage.setItem("ios_screen_mode", mode);
    // The screen transport is fixed for the session; changing it needs a
    // reconnect (the tunnel owns the video pipeline once connected).
    if (iosConnected) showToast?.(t("toasts.iosReconnectRequired"), "info");
  }

  async function handleIosInputMode(mode: string | null) {
    if (!mode || mode === iosInputMode) return;
    let ok = true;
    if (onIosInputModeChange) {
      ok = await onIosInputModeChange(mode as "bridge" | "tunnel");
    } else {
      localStorage.setItem("ios_input_mode", mode);
    }
    if (ok) setIosInputMode(mode as "bridge" | "tunnel");
  }

  // Whether the app starts with the system. The OS owns this (a login item,
  // not a stream setting), so the switch shows what the system reports and is
  // only moved once the write has been read back - a request the OS refuses
  // must not look like it worked. `null` until the first answer arrives.
  const [autostart, setAutostart] = useState<boolean | null>(null);
  useEffect(() => {
    let live = true;
    invoke<boolean>("autostart_enabled")
      .then((on) => { if (live) setAutostart(on); })
      .catch(() => { /* the switch stays off; toggling it reports the error */ });
    return () => { live = false; };
  }, []);

  async function handleAutostart(on: boolean) {
    try {
      setAutostart(await invoke<boolean>("set_autostart", { enabled: on }));
    } catch (e) {
      showToast?.(String(e));
    }
  }

  // File logging: off by default. A packaged build has no console, so this is
  // what makes a customer's report readable; Rust reads the choice at startup.
  const [fileLogging, setFileLogging] = useState<boolean | null>(null);
  const [logPath, setLogPath] = useState<string>("");
  useEffect(() => {
    let live = true;
    invoke<boolean>("file_logging_enabled")
      .then((on) => { if (live) setFileLogging(on); })
      .catch(() => { /* the switch stays off */ });
    invoke<string>("file_log_path")
      .then((path) => { if (live) setLogPath(path); })
      .catch(() => { });
    return () => { live = false; };
  }, []);

  async function handleFileLogging(on: boolean) {
    try {
      setFileLogging(await invoke<boolean>("set_file_logging", { enabled: on }));
    } catch (e) {
      showToast?.(String(e));
    }
  }

  async function handleUnicodeMethod(mode: string | null) {
    if (!mode || mode === unicodeMethod) return;
    setUnicodeMethod(mode as "clipboard" | "agentinput");
    setUnicodeTextMethod(mode);
    try {
      await invoke<string>("set_unicode_text_default", { method: mode });
    } catch (e) {
      showToast?.(String(e));
    }
  }

  // AgentInput: iOS text-input keyboard (needed for non-English text, which
  // the BLE-HID bridge cannot type). Its server only runs while the
  // AgentInput keyboard is active on the phone.
//   const [aiOpen, setAiOpen] = useState(false);  // Saved settings may still live under the pre-rename "remoboard_*" keys.
//   const [aiHost, setAiHost] = useState(
//     () => localStorage.getItem("agent_input_host") ?? localStorage.getItem("remoboard_host") ?? "",
//   );
//   const [aiPort, setAiPort] = useState(
//     () => localStorage.getItem("agent_input_port") ?? localStorage.getItem("remoboard_port") ?? "8787",
//   );
//   const [aiPin, setAiPin] = useState(
//     () => localStorage.getItem("agent_input_pin") ?? localStorage.getItem("remoboard_pin") ?? "",
//   );
//   const [aiConnecting, setAiConnecting] = useState(false);

//   async function connectAgentInput() {
//     const host = aiHost.trim();
//     if (!host) return;
//     await connectAgentInputArgs({ host, port: parseInt(aiPort, 10) || 8787 });
//     localStorage.setItem("agent_input_host", host);
//     localStorage.setItem("agent_input_port", aiPort);
//   }
// 
//   /** Connect over the USB cable (usbmuxd forward) - no shared WiFi needed. */
//   async function connectAgentInputUsb() {
//     await connectAgentInputArgs({ host: null, port: null });
//   }
// 
//   async function connectAgentInputArgs(args: { host: string | null; port: number | null }) {
//     setAiConnecting(true);
//     try {
//       await invoke<string>("set_agent_input", {
//         host: args.host,
//         port: args.port,
//         pin: aiPin.trim() || null,
//       });
//       if (aiPin.trim()) localStorage.setItem("agent_input_pin", aiPin.trim());
//       else localStorage.removeItem("agent_input_pin");
//       onAgentInputChange?.(true);
//       showToast?.(t("toasts.aiConnected"), "info");
//     } catch (e) {
//       onAgentInputChange?.(false);
//       showToast?.(t("toasts.aiConnectFailed", { error: String(e) }));
//     } finally {
//       setAiConnecting(false);
//     }
//   }
// 
//   async function disconnectAgentInput() {
//     try { await invoke("clear_agent_input"); } catch { }
//     onAgentInputChange?.(false);
//   }
// 
//   const checkMcpStatus = useCallback(async () => {
//     try {
//       const running = await invoke<boolean>("get_mcp_status");
//       setMcpRunning(running);
//     } catch { }
//   }, []);
// 
//   useEffect(() => {
//     checkMcpStatus();
//   }, [checkMcpStatus]);

//   async function handleMcpToggle(enabled: boolean) {
//     setMcpEnabled(enabled);
//     localStorage.setItem("mcp_enabled", String(enabled));
//     try {
//       if (enabled) {
//         await invoke("start_mcp_server", { port: mcpPort });
//       } else {
//         await invoke("stop_mcp_server");
//       }
//       await checkMcpStatus();
//     } catch { }
//   }

//   function handleCopyUrl() {
//     copyToClipboard(getMcpUrl(mcpPort));
//     setCopiedUrl(true);
//     setTimeout(() => setCopiedUrl(false), 2000);
//   }

//   function handleCopySnippet(key: string, text: string) {
//     copyToClipboard(text);
//     setCopiedSnippet(key);
//     setTimeout(() => setCopiedSnippet(null), 2000);
//   }

//   const mcpConfig = getMcpConfig(mcpPort);

  return (
    // The panel is the whole content of the settings window (src/main.tsx
    // renders it when the window label is "settings"), so it scrolls on its
    // own and needs no dialog shell: the OS title bar is the window chrome.
    // h-screen, not h-full: #root has no height of its own (the other screens
    // are h-screen too), and a percentage height against it would collapse to
    // the content, leaving the body's overflow-hidden to clip the last rows.
    <div className="h-screen w-screen bg-surface overflow-y-auto">
      <div className="px-5 py-4 border-b border-border/60">
        <div className="text-[10px] font-bold uppercase tracking-widest text-text-3 mb-3.5">{t("settings.language")}</div>
        <div className="flex gap-1.5">
          {APP_LANGUAGES.map((lang) => (
            <Button
              key={lang.code}
              variant={i18n.language === lang.code ? "default" : "outline"}
              size="sm"
              className="flex-1 text-[11px]"
              onClick={() => setAppLanguage(lang.code)}
            >
              {lang.label}
            </Button>
          ))}
        </div>
      </div>

      {/* App-level, like the language above it: not a stream parameter, so it
          is not gated on viewOnly and does not go through the settings
          object. */}
      <div className="px-5 py-4 border-b border-border/60">
        <div className="flex items-center justify-between">
          <span className="text-[13px] font-medium text-foreground">{t("settings.autostart")}</span>
          <Switch
            checked={autostart === true}
            disabled={autostart === null}
            onCheckedChange={handleAutostart}
          />
        </div>
        <div className="text-[11px] text-text-3 mt-2">{t("settings.autostartDesc")}</div>
      </div>

      {/* Off by default: a troubleshooting aid, and the only way to see what a
          packaged build logged when there is no console to read. */}
      <div className="px-5 py-4 border-b border-border/60">
        <div className="flex items-center justify-between">
          <span className="text-[13px] font-medium text-foreground">{t("settings.fileLog")}</span>
          <Switch
            checked={fileLogging === true}
            disabled={fileLogging === null}
            onCheckedChange={handleFileLogging}
          />
        </div>
        <div className="text-[11px] text-text-3 mt-2">{t("settings.fileLogDesc")}</div>
        {fileLogging && logPath && (
          <div className="text-[10px] font-mono text-text-3 mt-1 break-all select-all">{logPath}</div>
        )}
      </div>

      {!viewOnly && (
      <div className="px-5 py-4 border-b border-border/60">
        <div className="text-[10px] font-bold uppercase tracking-widest text-text-3 mb-3.5">{t("settings.presets")}</div>
        <div className="flex gap-1.5">
          {Object.keys(PRESETS).map((name) => (
            <Button
              key={name}
              variant={activePreset === name ? "default" : "outline"}
              size="sm"
              className="flex-1 text-[11px]"
              onClick={() => onApplyPreset(name)}
            >
              {t(`settings.presetNames.${name}`)}
            </Button>
          ))}
        </div>
        {/* The presets and the video settings below are stream parameters:
            changing one reconnects the device instead of being applied to the
            running stream. Audio and the adaptive switch are the two that take
            effect as they are toggled. */}
        <div className="text-[11px] text-text-3 mt-3">{t("settings.reconnectHint")}</div>
      </div>
      )}

      {!viewOnly && <Collapsible open={videoOpen} onOpenChange={setVideoOpen}>
        <SectionHeader open={videoOpen}>{t("settings.video")}</SectionHeader>
        <CollapsibleContent>
          <div className="px-5 py-4 border-b border-border/60">
            <div style={{ opacity: settings.adaptive ? 0.5 : 1, pointerEvents: settings.adaptive ? 'none' : 'auto' }}>
              <div className="flex items-center justify-between mb-3">
                <span className="text-[13px] font-medium text-foreground">{t("settings.resolution")}</span>
                <Select value={settings.max_size} onValueChange={(val) => onUpdateSetting("max_size", val as number)}>
                  <SelectTrigger className="h-8 text-xs">
                    <SelectValue>{settings.max_size === 0 ? t("common.native") : RESOLUTION_OPTIONS.find((o) => o.value === settings.max_size)?.label}</SelectValue>
                  </SelectTrigger>
                  <SelectContent>
                    {RESOLUTION_OPTIONS.map((o) => (
                      <SelectItem key={o.value} value={o.value}>
                        {o.value === 0 ? t("common.native") : o.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>

              <div className="flex items-center justify-between mb-3">
                <span className="text-[13px] font-medium text-foreground">{t("settings.maxFps")}</span>
                <span className="text-xs font-medium font-mono text-text-2 min-w-[50px] text-right">{settings.max_fps}</span>
              </div>
              <Slider
                className="mb-1"
                value={settings.max_fps}
                onValueChange={(val) => onUpdateSetting("max_fps", val as number)}
                min={15} max={120} step={5}
              />

              <div className="flex items-center justify-between mb-3 mt-3">
                <span className="text-[13px] font-medium text-foreground">{t("settings.bitrate")}</span>
                <span className="text-xs font-medium font-mono text-text-2 min-w-[50px] text-right">{(settings.video_bit_rate / 1000000).toFixed(0)} Mbps</span>
              </div>
              <Slider
                className="mb-1"
                value={settings.video_bit_rate}
                onValueChange={(val) => onUpdateSetting("video_bit_rate", val as number)}
                min={1000000} max={32000000} step={1000000}
              />
            </div>

            <div className="flex items-center justify-between mt-3">
              <span className="text-[13px] font-medium text-foreground">{t("settings.codec")}</span>
              <Select value={settings.video_codec} onValueChange={(val) => onUpdateSetting("video_codec", val as string)}>
                <SelectTrigger className="h-8 text-xs">
                  <SelectValue>{CODEC_OPTIONS.find((o) => o.value === settings.video_codec)?.label}</SelectValue>
                </SelectTrigger>
                <SelectContent>
                  {CODEC_OPTIONS.map((o) => (
                    <SelectItem key={o.value} value={o.value}>
                      {o.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>

            <div className="flex items-center justify-between mt-4">
              <span className="text-[13px] font-medium text-foreground">{t("settings.adaptiveQuality")}</span>
              <Switch
                checked={settings.adaptive}
                onCheckedChange={(checked) => onUpdateSetting("adaptive", checked)}
              />
            </div>
            <div className="text-[11px] text-text-3 mt-1">{t("settings.adaptiveDesc")}</div>
          </div>
        </CollapsibleContent>
      </Collapsible>}

      {!viewOnly && <Collapsible open={audioOpen} onOpenChange={setAudioOpen}>
        <SectionHeader open={audioOpen}>{t("settings.audio")}</SectionHeader>
        <CollapsibleContent>
          <div className="px-5 py-4 border-b border-border/60">
            <div className="flex items-center justify-between">
              <span className="text-[13px] font-medium text-foreground">{t("settings.forwardAudio")}</span>
              <Switch
                checked={settings.audio}
                onCheckedChange={(checked) => onUpdateSetting("audio", checked)}
              />
            </div>
            <div className="text-[11px] text-text-3 mt-2">{t("settings.requiresAndroid11")}</div>
          </div>
        </CollapsibleContent>
      </Collapsible>}

      <Collapsible open={storageOpen} onOpenChange={setStorageOpen}>
        <SectionHeader open={storageOpen}>{t("settings.storage")}</SectionHeader>
        <CollapsibleContent>
          <div className="px-5 py-4 border-b border-border/60">
            <div className="flex items-center justify-between">
              <span className="text-[13px] font-medium text-foreground">{t("settings.saveLocation")}</span>
              <Button
                variant="outline"
                size="sm"
                className="text-[11px] gap-1.5 [&_svg]:size-3.5"
                onClick={async () => {
                  const dir = await openDialog({ directory: true, multiple: false });
                  if (dir) {
                    setSavePath(dir);
                    localStorage.setItem("save_path", dir);
                  }
                }}
              >
                <FolderOpenIcon />
                {t("common.browse")}
              </Button>
            </div>
            {savePath ? (
              <div className="flex items-center justify-between mt-2.5">
                <code className="text-[11px] font-mono text-text-2 truncate flex-1 mr-2">{savePath}</code>
                <Button
                  variant="ghost"
                  size="xs"
                  className="text-[10px] text-text-3 shrink-0"
                  onClick={() => {
                    setSavePath("");
                    localStorage.removeItem("save_path");
                  }}
                >
                  {t("common.reset")}
                </Button>
              </div>
            ) : (
              <div className="text-[11px] text-text-3 mt-2">
                {t("settings.defaultSave", { path: defaultSaveDir ?? t("settings.documentsDir") })}
              </div>
            )}
          </div>
        </CollapsibleContent>
      </Collapsible>

      <Collapsible open={toolbarOpen} onOpenChange={setToolbarOpen}>
        <SectionHeader open={toolbarOpen}>{t("settings.toolbar")}</SectionHeader>
        <CollapsibleContent>
          <div className="px-5 py-4 border-b border-border/60">
            <div className="text-[11px] text-text-3 mb-3">{t("settings.toolbarDesc")}</div>
            <div className="flex flex-wrap gap-1.5">
              {QUICK_ACTIONS.filter((a) => {
                if (viewOnly) return a.id === "record";
                if (iosControllable) return ["record", "volume_up", "volume_down", "power"].includes(a.id);
                return true;
              }).map((action) => {
                const active = pinnedActions.includes(action.id);
                return (
                  <Button
                    key={action.id}
                    variant={active ? "default" : "outline"}
                    size="sm"
                    className="text-[11px]"
                    onClick={() => {
                      const next = active
                        ? pinnedActions.filter((a) => a !== action.id)
                        : [...pinnedActions, action.id];
                      onPinnedActionsChange(next);
                      localStorage.setItem("pinned_actions", JSON.stringify(next));
                    }}
                  >
                    {t(`settings.quickActions.${action.id}`)}
                  </Button>
                );
              })}
            </div>
          </div>
        </CollapsibleContent>
      </Collapsible>

      <Collapsible open={iosTransportOpen} onOpenChange={setIosTransportOpen}>
        <SectionHeader open={iosTransportOpen}>{t("settings.iosSection")}</SectionHeader>
        <CollapsibleContent>
          <div className="px-5 py-4 border-b border-border/60">
            <div className="text-[11px] text-text-3 mb-3">{t("settings.iosTransportDesc")}</div>
            <div className="flex items-center justify-between mb-3">
              <span className="text-[13px] font-medium text-foreground">{t("settings.iosScreenSource")}</span>
              <Select value={iosScreenMode} onValueChange={handleIosScreenMode}>
                <SelectTrigger className="h-8 text-xs w-[190px]">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="airplay">{t("settings.screenAirplay")}</SelectItem>
                  <SelectItem value="tunnel" disabled={tunnelBlocked}>{t("settings.screenTunnel")}</SelectItem>
                </SelectContent>
              </Select>
            </div>
            <div className="flex items-center justify-between">
              <span className="text-[13px] font-medium text-foreground">{t("settings.iosInputSource")}</span>
              <Select value={iosInputMode} onValueChange={handleIosInputMode}>
                <SelectTrigger className="h-8 text-xs w-[190px]">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="bridge">{t("settings.inputBridge")}</SelectItem>
                  <SelectItem value="tunnel" disabled={tunnelBlocked}>{t("settings.inputTunnel")}</SelectItem>
                </SelectContent>
              </Select>
            </div>
            {/* Non-ASCII text: the keyboard can only type ASCII, so the rest
                travels by clipboard or through the AgentInput keyboard. */}
            <div className="flex items-center justify-between mt-3">
              <span className="text-[13px] font-medium text-foreground">{t("settings.iosUnicodeMethod")}</span>
              <Select value={unicodeMethod} onValueChange={handleUnicodeMethod}>
                <SelectTrigger className="h-8 text-xs w-[190px]">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="clipboard">{t("settings.unicodeClipboard")}</SelectItem>
                  <SelectItem value="agentinput">{t("settings.unicodeAgentInput")}</SelectItem>
                </SelectContent>
              </Select>
            </div>
            <div className="text-[11px] text-text-3 mt-2">{t("settings.iosUnicodeHint")}</div>
          </div>
        </CollapsibleContent>
      </Collapsible>

      {/* Also not gated on `!viewOnly`: where the phone thinks it is has
          nothing to do with whether this session can send input. */}
      <LocationSettings open={locationOpen} onOpenChange={setLocationOpen} showToast={showToast} />

      {/* Not gated on `!viewOnly`: the copilot reads the screen, and an iOS
          view-only session has just as much need of a suggested reply. */}
      <CopilotSettings
        settings={settings}
        onUpdateSetting={onUpdateSetting}
        showToast={showToast}
      />

      {/* <Collapsible open={aiOpen} onOpenChange={setAiOpen}>
        <SectionHeader open={aiOpen}>{t("settings.aiSection")}</SectionHeader>
        <CollapsibleContent>
          <div className="px-5 py-4 border-b border-border/60">
            <div className="text-[11px] text-text-3 mb-3">
              {t("settings.aiDesc")}
            </div>
            <div className="flex items-center gap-2 mb-2">
              <input
                className="flex-1 min-w-0 h-8 px-2.5 bg-surface-2 border border-border rounded-lg text-[13px] text-foreground outline-none focus:border-brand placeholder:text-text-3"
                placeholder={t("settings.phoneIp")}
                value={aiHost}
                onChange={(e) => setAiHost(e.target.value)}
                spellCheck={false}
              />
              <input
                className="w-16 h-8 px-2.5 bg-surface-2 border border-border rounded-lg text-[13px] text-foreground outline-none focus:border-brand placeholder:text-text-3"
                placeholder="8787"
                value={aiPort}
                onChange={(e) => setAiPort(e.target.value)}
                spellCheck={false}
              />
            </div>
            <div className="flex items-center gap-2 mb-2.5">
              <input
                className="flex-1 min-w-0 h-8 px-2.5 bg-surface-2 border border-border rounded-lg text-[13px] text-foreground outline-none focus:border-brand placeholder:text-text-3"
                placeholder={t("settings.pin")}
                value={aiPin}
                onChange={(e) => setAiPin(e.target.value)}
                spellCheck={false}
              />
              <Button
                variant="outline"
                size="sm"
                className="shrink-0 h-8 text-[11px]"
                disabled={!aiHost.trim() || aiConnecting}
                onClick={connectAgentInput}
              >
                {aiConnecting ? t("common.connecting") : t("common.connect")}
              </Button>
              <Button
                variant="outline"
                size="sm"
                className="shrink-0 h-8 text-[11px]"
                disabled={!iosConnected || aiConnecting}
                onClick={connectAgentInputUsb}
                title={t("settings.remoUsbHint")}
              >
                {t("settings.remoUsb")}
              </Button>
              {agentInputReady && (
                <Button
                  variant="ghost"
                  size="sm"
                  className="shrink-0 h-8 text-[11px] text-text-3"
                  onClick={disconnectAgentInput}
                >
                  {t("common.forget")}
                </Button>
              )}
            </div>
            <div className="text-[11px] text-text-3">
              {agentInputReady ? t("settings.aiConfigured") : t("settings.aiNotConfigured")}
            </div>
          </div>
        </CollapsibleContent>
      </Collapsible>

      <Collapsible open={mcpOpen} onOpenChange={setMcpOpen}>
        <SectionHeader open={mcpOpen}>{t("settings.mcpSection")}</SectionHeader>
        <CollapsibleContent>
          <div className="px-5 py-4 border-b border-border/60">
            <div className="flex items-center justify-between">
              <span className="text-[13px] font-medium text-foreground">{t("settings.enabled")}</span>
              <Switch
                checked={mcpEnabled}
                onCheckedChange={handleMcpToggle}
              />
            </div>
            {mcpRunning && (
              <div className="text-[13px] font-medium text-foreground mt-1.5">{t("settings.runningOnPort", { port: mcpPort })}</div>
            )}
            {mcpEnabled && (
              <div className="mt-3.5">
                <p className="text-[11px] text-text-3 mb-3">{t("settings.letAi")}</p>
                <div className="flex items-center gap-2 mb-3">
                  <code className="flex-1 py-2 px-2.5 bg-surface-2 border border-border rounded-md text-xs font-mono text-foreground select-text">{getMcpUrl(mcpPort)}</code>
                  <Button variant="outline" size="xs" className="shrink-0 [&_svg]:size-[13px]" onClick={handleCopyUrl}>
                    <ClipboardDocumentIcon />
                    {copiedUrl ? t("common.copied") : t("settings.copyUrl")}
                  </Button>
                </div>
                <Button variant="ghost" className="flex items-center justify-between w-full h-auto p-0 mt-1 mb-1 text-foreground" onClick={() => setMcpInstructionsOpen(!mcpInstructionsOpen)}>
                  <span className="text-[13px] font-medium">{t("settings.setupInstructions")}</span>
                  <ChevronDownIcon className={cn("size-3.5 text-text-3 transition-transform duration-150", mcpInstructionsOpen && "rotate-180")} />
                </Button>
                {mcpInstructionsOpen && (
                  <div className="flex flex-col gap-2.5 mt-2.5">
                    <div className="border border-border rounded-lg overflow-hidden">
                      <div className="flex items-center justify-between py-1.5 px-2.5 bg-surface-2 border-b border-border">
                        <span className="text-[11px] font-semibold text-text-2">{t("settings.mcpClients")}</span>
                        <Button variant="outline" size="xs" className="shrink-0 text-[10px] [&_svg]:size-[11px]" onClick={() => handleCopySnippet("config", mcpConfig)}>
                          <ClipboardDocumentIcon />
                          {copiedSnippet === "config" ? t("common.copied") : t("common.copy")}
                        </Button>
                      </div>
                      <pre className="p-2.5 m-0 text-[11px] font-mono text-foreground bg-surface overflow-x-auto leading-relaxed select-text">{mcpConfig}</pre>
                    </div>
                  </div>
                )}
              </div>
            )}
          </div>
        </CollapsibleContent>
      </Collapsible> */}

      <button
        type="button"
        onClick={openHomepage}
        className="w-full flex items-center justify-between px-5 py-3 border-t border-border/60 text-[13px] font-medium text-foreground bg-transparent cursor-pointer hover:bg-surface-hover/50 transition-colors"
      >
        {t("settings.helpFeedback")}
        <ArrowTopRightOnSquareIcon className="size-4 text-text-3" />
      </button>

      <div className="px-5 py-4 text-[11px] text-text-3 flex items-center justify-between">
        <span>{t("settings.version", { name: t("common.appName"), version: APP_VERSION })}</span>
        <span>{t("settings.changesLive")}</span>
      </div>
    </div>
  );
}
