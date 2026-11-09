import { useState, useRef } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { useTranslation, Trans } from "react-i18next";
import {
  ArrowLeftIcon,
  PlayIcon,
  TrashIcon,
  PencilIcon,
  ArrowDownTrayIcon,
  ArrowUpTrayIcon,
  FolderIcon,
  CheckIcon,
  XMarkIcon,
  PlayCircleIcon,
  ArrowPathIcon,
  StopIcon,
} from "@heroicons/react/24/outline";
import {
  Dialog,
  DialogOverlay,
  DialogPortal,
  DialogTitle,
} from "@/components/ui/dialog";
import { Dialog as DialogPrimitive } from "@base-ui/react/dialog";
import { Button } from "@/components/ui/button";
import { cn, MOD } from "@/lib/utils";
import type { MacroInfo } from "../hooks/useMacro";
import macroIcon from "../assets/macro.png";


function MacroItemIcon() {
  return (
    <svg viewBox="0 0 24 25" fill="none" className="size-4 shrink-0 text-current">
      <path d="M11.41 2.068c-.57 0-1.08 0-1.55.17-.1.04-.19.08-.29.12-.46.22-.81.58-1.21.98L3.58 8.148c-.47.47-.88.88-1.11 1.43-.22.54-.22 1.13-.22 1.8v3.47c0 1.78 0 3.22.15 4.35.16 1.17.49 2.16 1.27 2.95.78.78 1.76 1.12 2.93 1.28 1.12.15 2.55.15 4.33.15s3.31 0 4.43-.15c-.49-1.1-1.51-2.09-2.61-2.52-1.66-.65-1.66-3.01 0-3.66 1.16-.46 2.22-1.52 2.67-2.67.66-1.66 3.01-1.66 3.66 0 .16.41.39.81.67 1.17V14.858c0-1.53 0-2.77-.11-3.75-.12-1.02-.37-1.89-.96-2.63-.22-.27-.46-.52-.73-.74-.73-.6-1.6-.85-2.61-.97-1.18-.11-2.4-.11-3.92-.11z" fill="currentColor" opacity="0.4" />
      <path fillRule="evenodd" clipRule="evenodd" d="M9.569 2.358c.09-.05.19-.09.29-.12.21-.07.42-.12.65-.14v1.99c0 1.36 0 2.01-.12 2.88-.12.9-.38 1.66-.98 2.26s-1.36.86-2.26.98c-.87.12-1.52.12-2.88.12H2.289c.03-.26.09-.51.18-.75.22-.54.64-.96 1.11-1.43l4.78-4.81c.4-.4.76-.77 1.21-.98zM17.919 23.118c-.24.61-1.09.61-1.33 0l-.04-.1a5.73 5.73 0 00-3.23-3.23l-.11-.04c-.6-.24-.6-1.1 0-1.33l.11-.04a5.73 5.73 0 003.23-3.23l.04-.1c.24-.61 1.09-.61 1.33 0l.04.1a5.73 5.73 0 003.23 3.23l.11.04c.6.24.6 1.1 0 1.33l-.11.04a5.73 5.73 0 00-3.23 3.23l-.04.1z" fill="currentColor" />
    </svg>
  );
}

interface MacrosScreenProps {
  macros: MacroInfo[];
  macrosDir: string;
  playingMacro: string | null;
  /** The macro looping right now, if any. */
  loopingMacro: string | null;
  onBack: () => void;
  onPlay: (name: string) => void;
  /** Play `name` again and again until Ctrl+E (or the button) stops it. */
  onLoop: (name: string) => void;
  onStopLoop: () => void;
  onDelete: (name: string) => void;
  onRename: (oldName: string, newName: string) => void;
  onReorder: (order: string[]) => void;
  onExport: (name: string) => void;
  onExportAll: () => void;
  onImport: () => void;
  onSetDir: (dir: string) => void;
  showToast: (msg: string, type?: "error" | "info") => void;
}

export function MacrosScreen({
  macros,
  macrosDir,
  playingMacro,
  loopingMacro,
  onBack,
  onPlay,
  onLoop,
  onStopLoop,
  onDelete,
  onRename,
  onExport,
  onExportAll,
  onImport,
  onSetDir,
  showToast,
}: MacrosScreenProps) {
  const { t } = useTranslation();
  const [editingName, setEditingName] = useState<string | null>(null);
  const [editValue, setEditValue] = useState("");
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  /** The macro a confirmation is asking about, and how it would be played. */
  const [pending, setPending] = useState<{ name: string; loop: boolean } | null>(null);
  const editInputRef = useRef<HTMLInputElement>(null);

  const handleStartEdit = (name: string) => {
    setEditingName(name);
    setEditValue(name);
    setTimeout(() => editInputRef.current?.select(), 0);
  };

  const handleConfirmEdit = () => {
    if (!editingName) return;
    const trimmed = editValue.trim();
    if (trimmed && trimmed !== editingName) {
      onRename(editingName, trimmed);
    }
    setEditingName(null);
  };

  const handlePickFolder = async () => {
    try {
      const selected = await open({ directory: true, multiple: false });
      if (selected) {
        onSetDir(selected as string);
        showToast(t("toasts.macrosFolderChanged"), "info");
      }
    } catch { }
  };

  const shortDir =
    macrosDir.length > 40
      ? "..." + macrosDir.slice(macrosDir.length - 37)
      : macrosDir;

  return (
    <div className="h-screen w-screen flex flex-col items-center justify-center bg-background relative animate-in fade-in duration-600 pt-[38px]">
      <div className="absolute top-0 left-0 right-0 h-[38px] drag-region bg-surface border-b border-border flex items-center justify-between" data-tauri-drag-region>
        <div className="flex items-center justify-between w-full pl-[78px] pr-2.5 no-drag pointer-events-none">
          <Button variant="ghost" size="icon-xs" className="pointer-events-auto text-text-3 [&_svg]:size-[15px]" onClick={onBack}>
            <ArrowLeftIcon />
          </Button>
          <div className="flex items-center gap-0.5 pointer-events-auto">
            <Button variant="ghost" size="icon-xs" className="text-text-3 [&_svg]:size-[15px]" onClick={onImport}>
              <ArrowUpTrayIcon />
            </Button>
            <Button variant="ghost" size="icon-xs" className="text-text-3 [&_svg]:size-[15px]" onClick={handlePickFolder}>
              <FolderIcon />
            </Button>
          </div>
        </div>
      </div>

      <div className="flex flex-col items-center gap-3 mb-2">
        <img src={macroIcon} alt={t("macrosScreen.title")} className="size-16 rounded-2xl" />
        <h1 className="text-[28px] font-bold tracking-tight">{t("macrosScreen.title")}</h1>
      </div>
      <p className="text-sm text-text-2 mb-10">{t("macrosScreen.subtitle")}</p>

      <div className="w-[360px] max-w-[90vw]">
        {macros.length > 0 && (
          <div className="flex items-center justify-between mb-1.5 px-2">
            <span className="text-[10px] font-semibold uppercase tracking-wider text-text-3">
              {t("macrosScreen.macros", { count: macros.length })}
            </span>
            <Button variant="ghost" size="xs" className="text-text-3 text-[10px] gap-[3px] [&_svg]:size-2.5" onClick={onExportAll}>
              <ArrowDownTrayIcon /> {t("common.export")}
            </Button>
          </div>
        )}

        {macros.length === 0 ? (
          <div className="text-center py-8 px-5 bg-surface border border-dashed border-border rounded-xl flex flex-col items-center">
            <PlayCircleIcon className="size-8 text-text-3 mb-3 opacity-50" />
            <p className="text-[13px] font-semibold text-text-2 mb-0.5">{t("macrosScreen.noMacros")}</p>
            <p className="text-xs text-text-3 leading-relaxed">
              {t("macrosScreen.recordHint", { mod: MOD })}
            </p>
            <Button variant="outline" size="sm" className="mt-3.5" onClick={onImport}>
              {t("macrosScreen.importFromFile")}
            </Button>
          </div>
        ) : (
          macros.map((m) => (
            <div
              key={m.name}
              className={cn(
                "bg-transparent border-none rounded-lg p-2.5 mb-0.5 cursor-default flex items-center gap-2.5",
                (playingMacro === m.name || loopingMacro === m.name) && "bg-brand-glow"
              )}
            >
              <div className="size-[34px] rounded-[7px] bg-surface-2 flex items-center justify-center text-text-2 shrink-0">
                <MacroItemIcon />
              </div>

              <div className="flex-1 min-w-0 flex flex-col gap-[3px]">
                {editingName === m.name ? (
                  <div className="flex items-center gap-1">
                    <input
                      ref={editInputRef}
                      className="flex-1 py-[3px] px-1.5 bg-surface-2 border border-brand rounded-[5px] text-foreground text-[13px] outline-none"
                      value={editValue}
                      onChange={(e) => setEditValue(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") handleConfirmEdit();
                        if (e.key === "Escape") setEditingName(null);
                      }}
                      autoFocus
                    />
                    <Button variant="ghost" size="icon-xs" className="text-success [&_svg]:size-3.5" onClick={handleConfirmEdit}>
                      <CheckIcon />
                    </Button>
                    <Button variant="ghost" size="icon-xs" className="text-text-3 [&_svg]:size-3.5" onClick={() => setEditingName(null)}>
                      <XMarkIcon />
                    </Button>
                  </div>
                ) : (
                  <>
                    <div className="text-[13px] font-semibold leading-tight truncate">{m.name}</div>
                    <div className="text-[10px] font-mono text-text-3 leading-tight">
                      {t("macrosScreen.events", { count: m.event_count })}
                    </div>
                  </>
                )}
              </div>

              {editingName !== m.name && (
                <div className="flex items-center gap-0.5 shrink-0">
                  <Button
                    variant="ghost"
                    size="icon-xs"
                    className="text-brand hover:bg-brand-glow [&_svg]:size-[15px]"
                    title={t("common.play")}
                    onClick={() => setPending({ name: m.name, loop: false })}
                    disabled={!!playingMacro || !!loopingMacro}
                  >
                    {playingMacro === m.name ? <div className="size-3.5 border-2 border-border border-t-brand rounded-full animate-spin" /> : <PlayIcon />}
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon-xs"
                    className={cn(
                      "hover:bg-brand-glow [&_svg]:size-[15px]",
                      loopingMacro === m.name ? "text-danger" : "text-brand"
                    )}
                    title={loopingMacro === m.name ? t("common.stop") : t("macrosScreen.loopPlay")}
                    onClick={() => {
                      if (loopingMacro === m.name) onStopLoop();
                      else setPending({ name: m.name, loop: true });
                    }}
                    disabled={!!playingMacro}
                  >
                    {loopingMacro === m.name ? <StopIcon /> : <ArrowPathIcon />}
                  </Button>
                  <Button variant="ghost" size="icon-xs" className="text-text-3 hover:text-foreground hover:bg-surface-2 [&_svg]:size-[15px]" onClick={() => handleStartEdit(m.name)}>
                    <PencilIcon />
                  </Button>
                  <Button variant="ghost" size="icon-xs" className="text-text-3 hover:text-foreground hover:bg-surface-2 [&_svg]:size-[15px]" onClick={() => onExport(m.name)}>
                    <ArrowDownTrayIcon />
                  </Button>
                  {confirmDelete === m.name ? (
                    <Button
                      variant="ghost"
                      size="icon-xs"
                      className="text-text-3 hover:text-danger hover:bg-danger-bg [&_svg]:size-[15px]"
                      onClick={() => { onDelete(m.name); setConfirmDelete(null); }}
                      onBlur={() => setConfirmDelete(null)}
                    >
                      <CheckIcon />
                    </Button>
                  ) : (
                    <Button
                      variant="ghost"
                      size="icon-xs"
                      className="text-text-3 hover:text-danger hover:bg-danger-bg [&_svg]:size-[15px]"
                      onClick={() => setConfirmDelete(m.name)}
                    >
                      <TrashIcon />
                    </Button>
                  )}
                </div>
              )}
            </div>
          ))
        )}

        <div className="flex items-center gap-1.5 py-1.5 px-2 mt-2 text-[10px] text-text-3 opacity-60 overflow-hidden whitespace-nowrap cursor-pointer [&_svg]:size-[11px] [&_svg]:shrink-0" onClick={handlePickFolder}>
          <FolderIcon />
          <span>{shortDir}</span>
        </div>
      </div>

      <Dialog open={!!pending} onOpenChange={(open) => { if (!open) setPending(null); }}>
        <DialogPortal>
          <DialogOverlay />
          <DialogPrimitive.Popup className="fixed top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2 w-[340px] max-w-[90vw] bg-surface border border-border rounded-[14px] z-50 shadow-lg animate-in fade-in zoom-in-95 duration-100">
            <DialogTitle className="text-[15px] font-semibold px-5 pt-5">
              {pending?.loop ? t("macrosScreen.loopPlay") : t("macrosScreen.playMacro")}
            </DialogTitle>
            <div className="px-5 py-4">
              <p className="text-xs text-muted-foreground leading-relaxed mb-3">
                <Trans
                  i18nKey={pending?.loop ? "macrosScreen.loopConfirm" : "macrosScreen.playConfirm"}
                  values={{ name: pending?.name ?? "", mod: MOD }}
                  components={{ s: <strong className="text-foreground" /> }}
                />
              </p>
              <div className="flex gap-2 mt-3 justify-end">
                <Button variant="outline" size="sm" onClick={() => setPending(null)}>
                  {t("common.cancel")}
                </Button>
                <Button
                  size="sm"
                  onClick={() => {
                    const ask = pending;
                    setPending(null);
                    if (!ask) return;
                    if (ask.loop) onLoop(ask.name);
                    else onPlay(ask.name);
                  }}
                >
                  {pending?.loop ? t("macrosScreen.loopStart") : t("common.play")}
                </Button>
              </div>
            </div>
          </DialogPrimitive.Popup>
        </DialogPortal>
      </Dialog>
    </div>
  );
}
