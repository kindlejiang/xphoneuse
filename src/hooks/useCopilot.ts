import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  CopilotAnalysis,
  CopilotCandidate,
  CopilotMsg,
  CopilotOutcome,
  Device,
  Settings,
} from "../types";
import { getDeviceKey, resolveCopilotConfig } from "../types";
import { t } from "../i18n";
import { useScreenSettle } from "./useScreenSettle";

/**
 * What the panel is doing right now.
 *
 * `reading` covers the whole first call (screenshot, OCR, judgement): they
 * happen inside one command, and the judgement is what makes it slow. The
 * judgement is on screen the moment that call returns, and only then does
 * `drafting` start - which is the reference's two-stage feel.
 */
export type CopilotPhase = "idle" | "reading" | "drafting" | "ready" | "error";

/** Minimum gap between two automatic analyses. A screen that keeps settling -
 *  an incoming call, a video, a chat with a typing indicator - must not turn
 *  into a stream of model calls. */
const AUTO_COOLDOWN_MS = 10_000;

/**
 * Whether two transcripts have a message in common.
 *
 * Consecutive transcripts of one conversation overlap - the new one begins with
 * the tail of the old - while a transcript that shares nothing with the last
 * one is a different conversation (another chat, another app). The panel uses
 * that to drop a judgement that was made about a conversation which is no
 * longer on screen: a draft written for the last person's message must not stay
 * clickable in front of the next person's.
 *
 * This is the coarse half of the rule the Rust side applies with the same
 * data (see `transcript::merge`). Duplicating it here is deliberate: the panel
 * only ever over-clears, which costs a re-analysis, whereas a missed clear
 * leaves a wrong draft on screen - the failure that matters.
 */
function sharesAnyMessage(a: CopilotMsg[], b: CopilotMsg[]): boolean {
  return a.some((left) =>
    b.some((right) => left.from === right.from && left.text.trim() === right.text.trim()),
  );
}

export interface CopilotHandle {
  /** The user has the copilot on. */
  enabled: boolean;
  /** Watching the mirrored canvas for a pause. */
  watching: boolean;
  phase: CopilotPhase;
  analysis: CopilotAnalysis | null;
  candidates: CopilotCandidate[];
  /** What the last screenshot was read as, for checking the parsing. */
  transcript: CopilotMsg[];
  /** What the conversation is called, when the app's header gave a name up.
   *  Null when it could not be read, which is the common case. */
  title: string | null;
  /** The failure to show, when the phase is `error`. */
  message: string;
  /** Run an analysis now, whatever the trigger rules say. */
  analyze: () => void;
  /** Forget the conversation and clear the panel. */
  reset: () => void;
  /** Panel opacity, 0.3-1. */
  opacity: number;
}

/**
 * The copilot's front half: when to analyze, and what the panel shows.
 *
 * The pixels only exist in this webview (WebCodecs decodes into the canvas), so
 * the trigger has to live here; everything after the screenshot happens in Rust
 * (`src-tauri/src/copilot.rs`), which owns the conversation between calls.
 */
export function useCopilot(options: {
  settings: Settings;
  connectedDevice: Device | null;
  streamStarted: boolean;
  canvasRef: React.RefObject<HTMLCanvasElement | null>;
  showToast: (message: string, type?: "error" | "info") => void;
}): CopilotHandle {
  const { settings, connectedDevice, streamStarted, canvasRef, showToast } = options;
  const config = useMemo(() => resolveCopilotConfig(settings), [settings]);

  const [phase, setPhase] = useState<CopilotPhase>("idle");
  const [analysis, setAnalysis] = useState<CopilotAnalysis | null>(null);
  const [candidates, setCandidates] = useState<CopilotCandidate[]>([]);
  const [transcript, setTranscript] = useState<CopilotMsg[]>([]);
  const [title, setTitle] = useState<string | null>(null);
  const [message, setMessage] = useState("");

  const configRef = useRef(config);
  configRef.current = config;
  const connectedRef = useRef(connectedDevice);
  connectedRef.current = connectedDevice;
  const runningRef = useRef(false);
  const lastRunRef = useRef(0);
  const deviceKeyRef = useRef<string | null>(null);
  /** The transcript the panel is currently showing a judgement for. A ref
   *  rather than the state: the analysis below runs after an await, where the
   *  captured state is already the previous render's. */
  const transcriptRef = useRef<CopilotMsg[]>([]);

  /** Apply a transcript that just came back, and forget the judgement if it is
   *  about a different conversation. Returns nothing: the callers only ever
   *  want the panel to be consistent. */
  const adoptTranscript = useCallback((next: CopilotMsg[], title: string | null) => {
    const previous = transcriptRef.current;
    if (previous.length > 0 && next.length > 0 && !sharesAnyMessage(previous, next)) {
      setAnalysis(null);
      setCandidates([]);
      setMessage("");
    }
    transcriptRef.current = next;
    setTranscript(next);
    setTitle(title);
  }, []);

  /** `auto` is set by the screen-settle trigger, never by the button: an
   *  automatic run only judges a screen whose newest message is the other
   *  person's (see `copilot_analyze` in Rust). */
  const analyze = useCallback(async (auto: boolean) => {
    const cfg = configRef.current;
    if (!cfg.enabled) return;
    if (runningRef.current) return;
    if (!connectedRef.current) {
      if (!auto) showToast(t("copilot.notConnected"), "info");
      return;
    }
    runningRef.current = true;
    lastRunRef.current = Date.now();
    setMessage("");
    setPhase("reading");
    try {
      const outcome = await invoke<CopilotOutcome>("copilot_analyze", { config: cfg, auto });
      adoptTranscript(outcome.transcript ?? [], outcome.title ?? null);
      // The judging route is optional, and the panel says which mode it is in:
      // without it there is no judgement to show, and the drafts come back in
      // the order the reply model put them in. A status line below (nothing
      // readable, nothing to answer) is more specific and overrides it.
      if (!cfg.judge.api_key.trim()) setMessage(t("copilot.noJudge"));
      if (outcome.status === "error") {
        setPhase("error");
        setMessage(outcome.message ?? t("copilot.failed"));
        return;
      }
      if (outcome.analysis) setAnalysis(outcome.analysis);
      if (outcome.status !== "ok") {
        // Nothing new to judge (or nothing readable). The judgement already
        // shown is still the right one, so it stays; only the wording changes.
        setPhase(outcome.analysis ? "ready" : "idle");
        setMessage(
          outcome.status === "no_chat"
            ? t("copilot.noChat")
            : outcome.status === "waiting"
              ? t("copilot.lastIsMine")
              : "",
        );
        return;
      }
      // The judgement is on screen from here; the drafts take longer and
      // arrive when they arrive.
      setPhase("drafting");
      const drafts = await invoke<CopilotCandidate[]>("copilot_draft", { config: cfg });
      setCandidates(drafts);
      setPhase("ready");
    } catch (e) {
      setPhase("error");
      setMessage(String(e));
    } finally {
      runningRef.current = false;
    }
  }, [showToast, adoptTranscript]);

  const analyzeRef = useRef(analyze);
  analyzeRef.current = analyze;

  const onSettled = useCallback(() => {
    const cfg = configRef.current;
    if (!cfg.enabled || !cfg.auto) return;
    if (runningRef.current) return;
    if (Date.now() - lastRunRef.current < AUTO_COOLDOWN_MS) return;
    void analyzeRef.current(true);
  }, []);

  // The effect's `enabled` has to be a plain boolean, or the interval is
  // rebuilt on every render and the baseline never survives long enough for
  // the screen to settle.
  const { watching } = useScreenSettle({
    canvasRef,
    enabled: config.enabled && !!connectedDevice && streamStarted,
    onSettled,
  });

  const reset = useCallback(() => {
    setAnalysis(null);
    setCandidates([]);
    setTranscript([]);
    transcriptRef.current = [];
    setTitle(null);
    setMessage("");
    setPhase("idle");
    lastRunRef.current = 0;
    void invoke("copilot_reset").catch(() => {});
  }, []);

  // A different phone is a different conversation: the history in Rust is
  // per-device, and the panel must not show the previous one's analysis.
  useEffect(() => {
    const key = connectedDevice ? getDeviceKey(connectedDevice) : null;
    if (deviceKeyRef.current !== null && deviceKeyRef.current !== key) reset();
    deviceKeyRef.current = key;
  }, [connectedDevice, reset]);

  // Turning the copilot off drops what was read: it is a conversation
  // transcript, and leaving it on screen after the feature is off would be
  // surprising.
  useEffect(() => {
    if (!config.enabled) {
      if (analysis || candidates.length || transcript.length) reset();
    }
  }, [config.enabled]);

  return {
    enabled: config.enabled,
    watching,
    phase,
    analysis,
    candidates,
    transcript,
    title,
    message,
    analyze: () => void analyzeRef.current(false),
    reset,
    opacity: config.opacity,
  };
}
