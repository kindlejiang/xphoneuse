export interface AndroidDevice {
  platform: "android";
  serial: string;
  model: string;
  state: string;
  wifi_available?: boolean;
}

export interface IosDevice {
  platform: "ios";
  udid: string;
  name: string;
  model: string;
  product_type: string;
  ios_version: string;
  /** "usb" | "network" | "unknown" */
  connection_type: string;
  /** Remote address when seen over Wi-Fi */
  network_ip?: string;
  paired: boolean;
  available: boolean;
}

export type Device = AndroidDevice | IosDevice;

export function isIosDevice(d: Device): d is IosDevice {
  return d.platform === "ios";
}

/** The USB CoreDevice tunnel (screen + input) needs iOS >= 27. */
export function iosSupportsTunnel(iosVersion: string): boolean {
  const major = parseInt(iosVersion.split(".")[0] ?? "", 10);
  return Number.isFinite(major) && major >= 27;
}

/** iOS below 27 has no USB tunnel, so driving it needs the Bluetooth dongle.
 *  An unreadable version answers false: a wrong "buy this" is worse than a
 *  missing hint. */
export function iosNeedsDongle(iosVersion: string): boolean {
  const major = parseInt((iosVersion ?? "").split(".")[0] ?? "", 10);
  return Number.isFinite(major) && major < 27;
}

/** Effective screen transport: stored preference, else the iOS-27+ tunnel
 *  default, else AirPlay. Devices below iOS 27 always resolve to AirPlay.
 *  An unknown version (no device connected) honors the stored value. */
export function resolveIosScreenMode(iosVersion?: string): "airplay" | "tunnel" {
  const stored = localStorage.getItem("ios_screen_mode");
  if (iosVersion === undefined) return stored === "tunnel" ? "tunnel" : "airplay";
  if (!iosSupportsTunnel(iosVersion)) return "airplay";
  return stored === "airplay" ? "airplay" : "tunnel";
}

/** Effective input transport: same rule as the screen, with the BLE-HID
 *  bridge in place of AirPlay. */
export function resolveIosInputMode(iosVersion?: string): "bridge" | "tunnel" {
  const stored = localStorage.getItem("ios_input_mode");
  if (iosVersion === undefined) return stored === "tunnel" ? "tunnel" : "bridge";
  if (!iosSupportsTunnel(iosVersion)) return "bridge";
  return stored === "bridge" ? "bridge" : "tunnel";
}

/** How non-ASCII text is delivered to an iPhone: the device clipboard (needs
 *  nothing installed; may show the Allow-Paste alert) or the AgentInput
 *  keyboard (must be installed and active; nothing pops up). Clipboard is the
 *  default, and this value is also pushed to Rust, which the MCP server reads
 *  per text call. */
export const UNICODE_TEXT_METHOD_KEY = "ios_unicode_method";

export function resolveUnicodeTextMethod(): "clipboard" | "agentinput" {
  return localStorage.getItem(UNICODE_TEXT_METHOD_KEY) === "agentinput"
    ? "agentinput"
    : "clipboard";
}

export function setUnicodeTextMethod(method: string): void {
  localStorage.setItem(UNICODE_TEXT_METHOD_KEY, method);
}

/** Stable per-device key (adb serial on Android, UDID on iOS). */
export function getDeviceKey(d: Device): string {
  return d.platform === "ios" ? d.udid : d.serial;
}

export interface Toast {
  id: number;
  message: string;
  type: "error" | "info";
}

export interface Settings {
  max_size: number;
  max_fps: number;
  video_bit_rate: number;
  video_codec: string;
  audio: boolean;
  adaptive: boolean;
  /** Conversation copilot. Optional so that a `stream_settings` written by an
   *  older version still loads, and so the presets below need no new field. */
  copilot?: CopilotConfig;
}

/**
 * Settings that are not stream parameters. Everything else in `Settings` is
 * handed to scrcpy, so editing it has to stop adaptive bitrate, leave the
 * active preset and schedule an Android reconnect - and the copilot's keys
 * must do none of those (see `updateSetting` in App.tsx).
 */
export const NON_STREAM_SETTING_KEYS: (keyof Settings)[] = ["audio", "adaptive", "copilot"];

/** A Jev (TypeSafe) decision route. Mirrors `JevRoute` in
 *  crates/xphoneuse-copilot/src/config.rs. */
export interface JevRoute {
  /** One of `JEV_PROVIDERS`; an unknown or empty value behaves like openrouter. */
  provider: string;
  /** Empty = the provider's own base URL. */
  base_url: string;
  api_key: string;
  /** Empty = the provider's own model. */
  model: string;
}

/** Any OpenAI-compatible chat route. Mirrors `ReplyRoute`. */
export interface ReplyRoute {
  base_url: string;
  /** Empty = reuse the judging route's key. */
  api_key: string;
  model: string;
}

/** How the screenshot is read. Mirrors `ScreenOptions`. */
export interface CopilotScreenOptions {
  /** Fraction of the height dropped from the top (status bar, title bar). */
  top_crop: number;
  /** Fraction of the height kept from the top (the rest is the input area). */
  bottom_crop: number;
  min_confidence: number;
  /** Vertical gap, as a multiple of the previous line's height, that starts a
   *  new bubble. */
  line_gap: number;
  /** Which side my own bubbles sit on. */
  me_on_right: boolean;
  /** Give up on telling the sides apart: everything is the other person's. */
  all_from_other: boolean;
}

export interface CopilotConfig {
  /** Frontend-only: nothing is called while this is off. */
  enabled: boolean;
  /** Frontend-only: analyze by itself when the screen settles, rather than
   *  only when the button is pressed. */
  auto: boolean;
  /** Frontend-only: panel opacity, 0.3-1. */
  opacity: number;
  candidate_count: number;
  /** Who the other person is, in the user's own words. */
  relationship: string;
  judge: JevRoute;
  reply: ReplyRoute;
  screen: CopilotScreenOptions;
}

/** One message read off the screen. */
export interface CopilotMsg {
  from: "me" | "other";
  text: string;
}

/** The Jev judgement. Every field is optional: a decision model that answers
 *  six of seven questions is still worth showing. */
export interface CopilotAnalysis {
  literal_question?: number;
  true_intent?: string;
  true_intent_confidence?: number;
  danger_level?: number;
  danger_max?: number;
  danger_confidence?: number;
  should_reply_now?: number;
  best_action?: string;
  other_needs?: string;
  tension_resolved?: number;
}

export interface CopilotCandidate {
  text: string;
  /** 0-1; 0 when the ranking call failed, 1 when there was nothing to rank. */
  score: number;
}

/** `copilot_analyze`'s answer. Everything but `ok` and `error` is a
 *  non-event rather than a failure: there was nothing new to judge. */
export interface CopilotOutcome {
  status: "ok" | "no_chat" | "waiting" | "unchanged" | "error";
  /** What the conversation is called, when the app's header gave a name up.
   *  Null means the name could not be read, not that the chat has none. */
  title: string | null;
  transcript: CopilotMsg[];
  analysis: CopilotAnalysis | null;
  message: string | null;
}

/** Jev providers, with the base URL and model the reference implementation
 *  ships (`crates/xphoneuse-copilot/src/config.rs`). The label is i18n key
 *  `copilot.providers.<id>`. */
export const JEV_PROVIDERS = [
  { id: "typesafe", base_url: "https://api.typesafe.ai", model: "jev-latest" },
  { id: "openrouter", base_url: "https://openrouter.ai/api", model: "typesafe/jev-1.13" },
  { id: "bocha", base_url: "https://jev.bocha.cn", model: "bocha-jev-v1" },
  { id: "vercel", base_url: "https://ai-gateway.vercel.sh/typesafe", model: "typesafe-ai/jev" },
  { id: "zen", base_url: "https://opencode.ai/zen", model: "jev-1.13" },
  { id: "custom", base_url: "", model: "" },
];

/** Drafting endpoints people actually have keys for. Choosing one fills in the
 *  base URL and the model and leaves the key alone. */
export const REPLY_PRESETS = [
  { id: "openrouter", base_url: "https://openrouter.ai/api/v1", model: "deepseek/deepseek-chat-v3.1" },
  { id: "deepseek", base_url: "https://api.deepseek.com/v1", model: "deepseek-chat" },
  { id: "dashscope", base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1", model: "qwen-plus" },
];

export const DEFAULT_COPILOT: CopilotConfig = {
  enabled: false,
  auto: true,
  opacity: 0.85,
  candidate_count: 3,
  relationship: "",
  judge: { provider: "openrouter", base_url: "", api_key: "", model: "" },
  reply: {
    base_url: "https://openrouter.ai/api/v1",
    api_key: "",
    model: "deepseek/deepseek-chat-v3.1",
  },
  screen: {
    top_crop: 0.12,
    bottom_crop: 0.84,
    min_confidence: 0.5,
    line_gap: 1.2,
    me_on_right: true,
    all_from_other: false,
  },
};

/** The stored copilot config, filled out to a whole one.
 *
 *  A deep merge on purpose: a config saved by an earlier version can be missing
 *  whole branches (a route, the screen options), and a shallow merge would hand
 *  Rust an object whose nested fields are undefined - which serde reads as
 *  "field absent", so the defaults have to be here too. */
export function resolveCopilotConfig(settings: Settings): CopilotConfig {
  const stored = settings.copilot;
  return {
    ...DEFAULT_COPILOT,
    ...stored,
    judge: { ...DEFAULT_COPILOT.judge, ...stored?.judge },
    reply: { ...DEFAULT_COPILOT.reply, ...stored?.reply },
    screen: { ...DEFAULT_COPILOT.screen, ...stored?.screen },
  };
}

export type MacroEvent =
  | { type: "touch"; action: string; x: number; y: number }
  | { type: "text"; text: string }
  | { type: "key"; keycode: number; action: string }
  | { type: "scroll"; x: number; y: number; dx: number; dy: number }
  | { type: "button"; button: string }
  | { type: "wait"; ms: number };

export interface MacroTimedEvent {
  timestamp_ms: number;
  event: MacroEvent;
}

export interface SavedMacro {
  name: string;
  events: MacroTimedEvent[];
}

export type FrameEvent =
  | {
      event: "config";
      data: {
        codec: string;
        description: string;
        /** Visible frame size; the iOS tunnel pads the coded picture with black borders. */
        width?: number;
        height?: number;
      };
    }
  | { event: "packet"; data: { key: boolean; data: string; timestamp: number } }
  | { event: "disconnected"; data: { reason: string } }
  | { event: "agent_input"; data: { host: string } };

export type Screen = "welcome" | "another";

export function getDeviceNickname(serial: string): string | null {
  return localStorage.getItem(`device_nickname_${serial}`);
}

export function setDeviceNickname(serial: string, name: string) {
  if (name.trim()) {
    localStorage.setItem(`device_nickname_${serial}`, name.trim());
  } else {
    localStorage.removeItem(`device_nickname_${serial}`);
  }
}

export function getDeviceDisplayName(device: Device): string {
  if (isIosDevice(device)) {
    return getDeviceNickname(device.udid) || device.name || device.model;
  }
  return getDeviceNickname(device.serial) || device.model;
}

export type ThemePreference = "light" | "dark" | "auto";

export const PRESETS: Record<string, Settings> = {
  performance: { max_size: 720, max_fps: 30, video_bit_rate: 2000000, video_codec: "h264", audio: false, adaptive: false },
  balanced: { max_size: 1024, max_fps: 60, video_bit_rate: 8000000, video_codec: "h264", audio: false, adaptive: false },
  quality: { max_size: 1920, max_fps: 60, video_bit_rate: 24000000, video_codec: "h264", audio: false, adaptive: false },
};

export const RESOLUTION_OPTIONS = [
  { value: 480, label: "480p" },
  { value: 720, label: "720p" },
  { value: 1024, label: "1024p" },
  { value: 1280, label: "1280p" },
  { value: 1920, label: "1920p" },
  { value: 0, label: "Native" },
];

export const CODEC_OPTIONS = [
  { value: "h264", label: "H.264" },
];

export type QuickActionId = "record" | "mute" | "volume_up" | "volume_down" | "power" | "macro-toggle" | "rotate";

/** Toolbar quick actions; labels are translated via i18n (`settings.quickActions.<id>`). */
export const QUICK_ACTIONS: { id: QuickActionId }[] = [
  { id: "record" },
  { id: "mute" },
  { id: "volume_up" },
  { id: "volume_down" },
  { id: "power" },
  { id: "macro-toggle" },
  { id: "rotate" },
];

export const DEFAULT_PINNED_ACTIONS: QuickActionId[] = [];

export interface AdaptiveTier {
  max_size: number;
  max_fps: number;
  video_bit_rate: number;
}

export const ADAPTIVE_TIERS: AdaptiveTier[] = [
  { max_size: 720, max_fps: 30, video_bit_rate: 2000000 },
  { max_size: 720, max_fps: 60, video_bit_rate: 4000000 },
  { max_size: 1024, max_fps: 60, video_bit_rate: 8000000 },
  { max_size: 1280, max_fps: 60, video_bit_rate: 12000000 },
  { max_size: 1920, max_fps: 60, video_bit_rate: 24000000 },
  { max_size: 1920, max_fps: 120, video_bit_rate: 32000000 },
];
