import { useState } from "react";
import { useTranslation } from "react-i18next";
import {
  ArrowPathIcon,
  ChatBubbleLeftRightIcon,
  ChevronDownIcon,
  ChevronUpIcon,
  ClipboardDocumentCheckIcon,
  ClipboardDocumentIcon,
  ExclamationTriangleIcon,
  SparklesIcon,
} from "@heroicons/react/24/outline";
import { Button } from "@/components/ui/button";
import type { CopilotCandidate } from "../types";
import type { CopilotHandle } from "../hooks/useCopilot";

/**
 * The conversation copilot's panel: a translucent strip along the top of the
 * mirror that says what Jev made of the conversation and offers drafted
 * replies.
 *
 * It never sends anything. A candidate goes into the app's own input box, and
 * the user presses send.
 *
 * Three details are load-bearing:
 *
 * * it floats at the **top**, not above the input bar: the bar it fills is at
 *   the bottom, and a panel there covers the one control the user needs to
 *   reach after picking a candidate;
 * * every interactive element stops propagation of `onKeyDown` - the canvas
 *   viewport's handler types any single character straight into the phone, so
 *   a focused button would otherwise put a space on the phone's screen;
 * * the panel is positioned absolutely rather than laid out in flow: the
 *   window height is computed from the chrome around the canvas
 *   (`mirrorChromeHeight`), and a panel that took part in the flex layout would
 *   silently make the mirror window the wrong size.
 */

interface CopilotPanelProps {
  copilot: CopilotHandle;
  /** Put a candidate in the app's input box. Never sends it. */
  onFill: (text: string) => void;
}

/** The danger scale's three bands, from the reference implementation. */
function dangerColor(level: number, max: number): string {
  const ratio = max > 0 ? level / max : 0;
  if (ratio >= 0.6) return "#DC2626";
  if (ratio >= 0.3) return "#D97706";
  return "#16A34A";
}

function percent(value: number | undefined): string {
  return value === undefined ? "" : `${Math.round(value * 100)}%`;
}

export function CopilotPanel({ copilot, onFill }: CopilotPanelProps) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(true);
  const [showTranscript, setShowTranscript] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);

  if (!copilot.enabled) return null;

  const { analysis, candidates, phase, transcript } = copilot;
  const working = phase === "reading" || phase === "drafting";
  const status = (() => {
    switch (phase) {
      case "reading": return t("copilot.reading");
      case "drafting": return t("copilot.drafting");
      // The detail is in the red block below; the line stays short.
      case "error": return t("copilot.failed");
      default:
        // "Nothing on this screen" and "the last message is yours" arrive as a
        // message rather than as a phase.
        if (copilot.message) return copilot.message;
        return analysis
          ? t("copilot.ready")
          : copilot.watching
            ? t("copilot.watching")
            : t("copilot.manualHint");
    }
  })();

  const level = analysis?.danger_level;
  const max = analysis?.danger_max ?? 9;
  const dotColor = level === undefined ? undefined : dangerColor(level, max);

  const copy = (text: string) => {
    navigator.clipboard.writeText(text).then(
      () => {
        setCopied(text);
        setTimeout(() => setCopied((current) => (current === text ? null : current)), 1500);
      },
      () => { /* the clipboard is a convenience; failing it is not worth a toast */ },
    );
  };

  const intent = analysis?.true_intent;
  const needs = analysis?.other_needs;
  const action = analysis?.best_action;

  return (
    // `pointer-events-none` on the wrapper, `auto` on the card: the strip spans
    // the window, and clicks beside the card must still reach the mirror.
    <div className="absolute inset-x-0 top-0 z-20 px-2 pt-2 pointer-events-none">
      <div className="relative overflow-hidden rounded-xl border border-border shadow-lg pointer-events-auto">
        {/* The opacity setting fades the backdrop, not the text: a panel whose
            content fades with it would be unreadable at the low end. */}
        <div
          className="absolute inset-0 bg-surface backdrop-blur-xl"
          style={{ opacity: copilot.opacity }}
        />
        <div className="relative">
          <div
            className="flex items-center gap-2 px-2.5 py-1.5"
            onMouseDown={(e) => e.stopPropagation()}
            onWheel={(e) => e.stopPropagation()}
            onKeyDown={(e) => e.stopPropagation()}
          >
            <SparklesIcon className="size-4 shrink-0 text-brand" />
            <span className="text-[12px] font-semibold text-foreground shrink-0">
              {t("copilot.title")}
            </span>
            {dotColor && (
              <span
                className="size-2 rounded-full shrink-0"
                style={{ backgroundColor: dotColor }}
                title={t("copilot.danger", { level: Math.round(level ?? 0), max: Math.round(max) })}
              />
            )}
            {/* Which conversation this is, when the app's header gave its name
                up (see `transcript::read_title` - it answers only when it is
                sure). It doubles as the visible answer to "did it read the
                right chat?", and disappears when it read none. */}
            {copilot.title && (
              <span
                className="shrink-0 max-w-[38%] truncate rounded-md bg-surface-2 px-1.5 text-[11px] text-text-2"
                title={copilot.title}
              >
                {copilot.title}
              </span>
            )}
            <span className="text-[11px] text-text-2 truncate flex-1">{status}</span>
            {working && (
              <span className="size-3 shrink-0 border-2 border-border border-t-brand rounded-full animate-spin" />
            )}
            <button
              className="flex items-center justify-center size-5 rounded-md text-text-2 hover:text-foreground hover:bg-surface-2 transition-colors cursor-pointer shrink-0"
              onClick={() => void copilot.analyze()}
              disabled={working}
              title={t("copilot.reanalyze")}
              aria-label={t("copilot.reanalyze")}
            >
              <ArrowPathIcon className="size-3.5" />
            </button>
            <button
              className="flex items-center justify-center size-5 rounded-md text-text-2 hover:text-foreground hover:bg-surface-2 transition-colors cursor-pointer shrink-0"
              onClick={() => setExpanded((v) => !v)}
              title={expanded ? t("copilot.collapse") : t("copilot.expand")}
              aria-label={expanded ? t("copilot.collapse") : t("copilot.expand")}
            >
              {expanded ? <ChevronDownIcon className="size-3.5" /> : <ChevronUpIcon className="size-3.5" />}
            </button>
          </div>

          {expanded && (
            <div
              className="px-2.5 pb-2 max-h-[46vh] overflow-y-auto"
              onMouseDown={(e) => e.stopPropagation()}
              onWheel={(e) => e.stopPropagation()}
              onKeyDown={(e) => e.stopPropagation()}
            >
              {phase === "error" && (
                <div className="flex items-start gap-1.5 text-[11px] text-[#DC2626] pb-1.5">
                  <ExclamationTriangleIcon className="size-3.5 shrink-0 mt-px" />
                  <span className="break-all">{copilot.message}</span>
                </div>
              )}

              {analysis ? (
                <>
                  <div className="flex items-baseline gap-1.5">
                    <span className="text-[13px] font-semibold text-foreground">
                      {intent ? t(`copilot.intent.${intent}`, intent) : t("copilot.noIntent")}
                    </span>
                    {analysis.true_intent_confidence !== undefined && (
                      <span className="text-[11px] text-text-2">
                        {percent(analysis.true_intent_confidence)}
                      </span>
                    )}
                  </div>
                  <div className="text-[11px] text-text-2 leading-relaxed mt-0.5">
                    {needs && (
                      <span>
                        {t("copilot.needsLabel")} {t(`copilot.needs.${needs}`, needs)}
                      </span>
                    )}
                    {action && (
                      <span>
                        {needs ? " · " : ""}
                        {t("copilot.actionLabel")} {t(`copilot.action.${action}`, action)}
                      </span>
                    )}
                    {analysis.should_reply_now !== undefined && (
                      <span>
                        {" · "}
                        {analysis.should_reply_now >= 0.5
                          ? t("copilot.canBeSubstantive")
                          : t("copilot.holdLine")}
                      </span>
                    )}
                  </div>
                </>
              ) : (
                phase !== "error" && <div className="text-[11px] text-text-2 pb-1">{t("copilot.empty")}</div>
              )}

              {candidates.length > 0 && (
                <div className="mt-2">
                  <div className="text-[10px] font-medium text-text-2 uppercase tracking-wide mb-1">
                    {analysis ? t("copilot.candidates") : t("copilot.candidatesUnranked")}
                  </div>
                  <div className="flex flex-col gap-1">
                    {candidates.map((candidate, index) => (
                      <CandidateRow
                        key={`${index}-${candidate.text}`}
                        rank={index + 1}
                        candidate={candidate}
                        copied={copied === candidate.text}
                        onCopy={() => copy(candidate.text)}
                        onFill={() => onFill(candidate.text)}
                      />
                    ))}
                  </div>
                </div>
              )}

              {transcript.length > 0 && (
                <div className="mt-2">
                  <button
                    className="flex items-center gap-1 text-[10px] font-medium text-text-2 uppercase tracking-wide hover:text-foreground transition-colors cursor-pointer"
                    onClick={() => setShowTranscript((v) => !v)}
                  >
                    <ChatBubbleLeftRightIcon className="size-3" />
                    {t("copilot.transcript")} ({transcript.length})
                  </button>
                  {showTranscript && (
                    <div className="mt-1 flex flex-col gap-0.5">
                      {transcript.map((msg, index) => (
                        <div key={index} className="text-[11px] leading-snug">
                          <span className={msg.from === "me" ? "text-brand" : "text-text-2"}>
                            {msg.from === "me" ? t("copilot.me") : t("copilot.other")}
                          </span>
                          <span className="text-foreground"> {msg.text}</span>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function CandidateRow({
  rank,
  candidate,
  copied,
  onCopy,
  onFill,
}: {
  rank: number;
  candidate: CopilotCandidate;
  copied: boolean;
  onCopy: () => void;
  onFill: () => void;
}) {
  const { t } = useTranslation();
  return (
    <div className="flex items-start gap-2 px-2 py-1.5 rounded-lg bg-surface-2/80 border border-border">
      <div className="flex flex-col items-center shrink-0 w-8 pt-0.5">
        <span className="text-[10px] font-mono text-text-2">#{rank}</span>
        {candidate.score > 0 && (
          <span className="text-[10px] font-mono text-brand">{Math.round(candidate.score * 100)}%</span>
        )}
      </div>
      <span className="flex-1 text-[12px] text-foreground leading-snug break-words">{candidate.text}</span>
      <div className="flex items-center gap-0.5 shrink-0">
        <button
          className="flex items-center justify-center size-6 rounded-md text-text-2 hover:text-foreground hover:bg-surface transition-colors cursor-pointer"
          onClick={onCopy}
          title={t("copilot.copy")}
          aria-label={t("copilot.copy")}
        >
          {copied ? (
            <ClipboardDocumentCheckIcon className="size-3.5 text-[#16A34A]" />
          ) : (
            <ClipboardDocumentIcon className="size-3.5" />
          )}
        </button>
        <Button size="sm" className="h-6 px-2 text-[11px]" onClick={onFill} title={t("copilot.fillHint")}>
          {t("copilot.fill")}
        </Button>
      </div>
    </div>
  );
}
