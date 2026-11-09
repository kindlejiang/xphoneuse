import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { ChevronDownIcon } from "@heroicons/react/24/outline";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Slider } from "@/components/ui/slider";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import type { CopilotConfig, Settings } from "../types";
import { JEV_PROVIDERS, REPLY_PRESETS, resolveCopilotConfig } from "../types";

/**
 * Settings → Conversation copilot.
 *
 * Two routes, and they are different kinds of service: `judge` is a Jev
 * decision endpoint (it answers questions and writes nothing) and `reply` is
 * any OpenAI-compatible chat endpoint (it writes the drafts). Keeping them
 * apart is what makes the provider presets mean something.
 *
 * Everything here goes through the normal settings path, so it is stored in
 * localStorage with the rest of the settings and reaches the app's own window
 * through the settings bridge. The keys never leave this machine.
 */
export function CopilotSettings({
  settings,
  onUpdateSetting,
  showToast,
}: {
  settings: Settings;
  onUpdateSetting: <K extends keyof Settings>(key: K, value: Settings[K]) => void;
  showToast?: (msg: string, type?: "error" | "info") => void;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [testing, setTesting] = useState<"judge" | "reply" | null>(null);

  const config = resolveCopilotConfig(settings);
  const patch = (changes: Partial<CopilotConfig>) => {
    onUpdateSetting("copilot", { ...config, ...changes });
  };
  const patchJudge = (changes: Partial<CopilotConfig["judge"]>) => {
    patch({ judge: { ...config.judge, ...changes } });
  };
  const patchReply = (changes: Partial<CopilotConfig["reply"]>) => {
    patch({ reply: { ...config.reply, ...changes } });
  };
  const patchScreen = (changes: Partial<CopilotConfig["screen"]>) => {
    patch({ screen: { ...config.screen, ...changes } });
  };

  const judgePreset = JEV_PROVIDERS.find((p) => p.id === config.judge.provider);
  const replyPresetId =
    REPLY_PRESETS.find(
      (p) => p.base_url === config.reply.base_url && p.model === config.reply.model,
    )?.id ?? "custom";

  const test = async (route: "judge" | "reply") => {
    setTesting(route);
    try {
      const detail = await invoke<string>("copilot_test_route", { config, route });
      showToast?.(t("settings.copilotTestOk", { detail }), "info");
    } catch (e) {
      showToast?.(t("settings.copilotTestFailed", { error: String(e) }));
    } finally {
      setTesting(null);
    }
  };

  const field = "h-8 text-xs";
  const label = "text-[13px] font-medium text-foreground";
  const row = "flex items-center justify-between gap-3";

  return (
    <Collapsible open={open} onOpenChange={setOpen}>
      <CollapsibleTrigger className="flex items-center justify-between w-full px-5 py-3 border-b border-border/60 cursor-pointer hover:bg-surface-hover/50 transition-colors">
        <span className="text-[10px] font-bold uppercase tracking-widest text-text-3">
          {t("settings.copilotSection")}
        </span>
        <ChevronDownIcon
          className={cn("size-3 text-text-3 transition-transform duration-200", open && "rotate-180")}
        />
      </CollapsibleTrigger>
      <CollapsibleContent>
        <div className="px-5 py-4 border-b border-border/60 flex flex-col gap-3">
          <div className="text-[11px] text-text-3">{t("settings.copilotDesc")}</div>

          <div className={row}>
            <span className={label}>{t("settings.copilotEnabled")}</span>
            <Switch checked={config.enabled} onCheckedChange={(v) => patch({ enabled: v })} />
          </div>
          <div className={row}>
            <span className={label}>{t("settings.copilotAuto")}</span>
            <Switch
              checked={config.auto}
              disabled={!config.enabled}
              onCheckedChange={(v) => patch({ auto: v })}
            />
          </div>
          <div>
            <div className={cn(row, "mb-2")}>
              <span className={label}>{t("settings.copilotOpacity")}</span>
              <span className="text-[11px] text-text-3 font-mono">
                {Math.round(config.opacity * 100)}%
              </span>
            </div>
            <Slider
              value={config.opacity}
              min={0.3}
              max={1}
              step={0.05}
              onValueChange={(value) => patch({ opacity: value as number })}
            />
          </div>
          <div className={row}>
            <span className={label}>{t("settings.copilotCandidates")}</span>
            <Select
              value={String(config.candidate_count)}
              onValueChange={(v) => { if (v) patch({ candidate_count: Number(v) }); }}
            >
              <SelectTrigger className={cn(field, "w-[190px]")}>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {[1, 2, 3, 4, 5].map((n) => (
                  <SelectItem key={n} value={String(n)}>
                    {n}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div>
            <div className={cn(label, "mb-1.5")}>{t("settings.copilotRelationship")}</div>
            <Input
              className={field}
              value={config.relationship}
              placeholder={t("settings.copilotRelationshipPlaceholder")}
              onChange={(e) => patch({ relationship: e.target.value })}
            />
          </div>

          {/* --- judging route ------------------------------------------- */}
          <div className="text-[10px] font-bold uppercase tracking-widest text-text-3 mt-1">
            {t("settings.copilotJudge")}
          </div>
          {/* Optional on purpose: without it the drafts still come, ordered by
              the model that wrote them. */}
          <p className="text-[11px] text-text-3 -mt-1 leading-relaxed">
            {t("settings.copilotJudgeOptional")}
          </p>
          <div className={row}>
            <span className={label}>{t("settings.copilotProvider")}</span>
            <Select
              value={config.judge.provider || "openrouter"}
              onValueChange={(v) => { if (v) patchJudge({ provider: v }); }}
            >
              <SelectTrigger className={cn(field, "w-[190px]")}>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {JEV_PROVIDERS.map((provider) => (
                  <SelectItem key={provider.id} value={provider.id}>
                    {t(`copilot.providers.${provider.id}`)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          {/* A custom provider takes the URL verbatim (no path is appended), so
              the field is only meaningful for it; for the presets the base URL
              and model are shown as placeholders and an empty value means
              "use the preset". */}
          {config.judge.provider === "custom" && (
            <Input
              className={field}
              value={config.judge.base_url}
              placeholder="https://example.com/v1/systemone"
              onChange={(e) => patchJudge({ base_url: e.target.value })}
            />
          )}
          {config.judge.provider !== "custom" && (
            <Input
              className={field}
              value={config.judge.base_url}
              placeholder={judgePreset?.base_url}
              onChange={(e) => patchJudge({ base_url: e.target.value })}
            />
          )}
          <Input
            className={field}
            value={config.judge.model}
            placeholder={judgePreset?.model || t("settings.copilotModel")}
            onChange={(e) => patchJudge({ model: e.target.value })}
          />
          <Input
            className={field}
            type="password"
            value={config.judge.api_key}
            placeholder={t("settings.copilotApiKey")}
            onChange={(e) => patchJudge({ api_key: e.target.value })}
          />
          <Button
            variant="outline"
            size="sm"
            className="h-8 self-start"
            disabled={testing !== null}
            onClick={() => void test("judge")}
          >
            {testing === "judge" ? t("settings.copilotTesting") : t("settings.copilotTest")}
          </Button>

          {/* --- drafting route ------------------------------------------ */}
          <div className="text-[10px] font-bold uppercase tracking-widest text-text-3 mt-1">
            {t("settings.copilotReply")}
          </div>
          <div className={row}>
            <span className={label}>{t("settings.copilotReplyPreset")}</span>
            <Select
              value={replyPresetId}
              onValueChange={(id) => {
                const preset = REPLY_PRESETS.find((p) => p.id === id);
                if (preset) patchReply({ base_url: preset.base_url, model: preset.model });
              }}
            >
              <SelectTrigger className={cn(field, "w-[190px]")}>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {REPLY_PRESETS.map((preset) => (
                  <SelectItem key={preset.id} value={preset.id}>
                    {preset.id}
                  </SelectItem>
                ))}
                <SelectItem value="custom">{t("copilot.providers.custom")}</SelectItem>
              </SelectContent>
            </Select>
          </div>
          <Input
            className={field}
            value={config.reply.base_url}
            placeholder="https://openrouter.ai/api/v1"
            onChange={(e) => patchReply({ base_url: e.target.value })}
          />
          <Input
            className={field}
            value={config.reply.model}
            placeholder="deepseek/deepseek-chat-v3.1"
            onChange={(e) => patchReply({ model: e.target.value })}
          />
          <Input
            className={field}
            type="password"
            value={config.reply.api_key}
            placeholder={t("settings.copilotApiKey")}
            onChange={(e) => patchReply({ api_key: e.target.value })}
          />
          <div className="text-[11px] text-text-3">{t("settings.copilotReplyHint")}</div>
          <Button
            variant="outline"
            size="sm"
            className="h-8 self-start"
            disabled={testing !== null}
            onClick={() => void test("reply")}
          >
            {testing === "reply" ? t("settings.copilotTesting") : t("settings.copilotTest")}
          </Button>

          {/* --- telling the sides apart ---------------------------------- */}
          <div className="text-[10px] font-bold uppercase tracking-widest text-text-3 mt-1">
            {t("settings.copilotSides")}
          </div>
          <div className={row}>
            <span className={label}>{t("settings.copilotMeOnRight")}</span>
            <Switch
              checked={config.screen.me_on_right}
              onCheckedChange={(v) => patchScreen({ me_on_right: v })}
            />
          </div>
          <div className={row}>
            <span className={label}>{t("settings.copilotAllOther")}</span>
            <Switch
              checked={config.screen.all_from_other}
              onCheckedChange={(v) => patchScreen({ all_from_other: v })}
            />
          </div>

          <div className="text-[11px] text-text-3">{t("settings.copilotKeysHint")}</div>
        </div>
      </CollapsibleContent>
    </Collapsible>
  );
}
