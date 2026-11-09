import { useTranslation } from "react-i18next";
import { Dialog, DialogPortal, DialogOverlay, DialogTitle } from "@/components/ui/dialog";
import { Dialog as DialogPrimitive } from "@base-ui/react/dialog";
import { Button } from "@/components/ui/button";
import type { UpdatePrompt } from "../hooks/useUpdater";

/**
 * The "a new version is out" question.
 *
 * The update is held back until it is answered: the download is tens of
 * megabytes, and on Windows the installer closes the app - that is the user's
 * call, not something to start because a check ran. The release notes come from
 * the feed itself (the text written at publish time), which is the only place
 * the app can learn what actually changed.
 */
export function UpdateDialog({
  prompt,
  onLater,
  onInstall,
}: {
  prompt: UpdatePrompt | null;
  onLater: () => void;
  onInstall: () => void;
}) {
  const { t } = useTranslation();
  if (!prompt) return null;

  return (
    <Dialog open onOpenChange={(open) => !open && onLater()}>
      <DialogPortal>
        <DialogOverlay />
        <DialogPrimitive.Popup className="fixed left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 w-[min(92vw,380px)] bg-surface border border-border rounded-2xl shadow-xl z-51 p-5 animate-in fade-in zoom-in-[0.98] duration-200">
          <DialogTitle className="text-[15px] font-bold text-foreground">
            {t("update.title", { version: prompt.version })}
          </DialogTitle>
          <p className="mt-1 text-xs text-muted-foreground">
            {t("update.current", { current: prompt.currentVersion })}
          </p>

          {prompt.notes && (
            <>
              {/* Bold, but small: the label says what the text below is, and
                  stays out of the way of the question the dialog is asking.
                  The notes themselves carry no emphasis beyond that. */}
              <p className="mt-3 text-xs font-semibold text-foreground">{t("update.whatsNew")}</p>
              {/* Release notes are free text: they keep their own line breaks,
                  and a long one scrolls rather than growing the dialog. */}
              <div className="mt-0.5 max-h-40 overflow-y-auto">
                <p className="text-[13px] leading-relaxed text-muted-foreground whitespace-pre-wrap break-words">
                  {prompt.notes}
                </p>
              </div>
            </>
          )}

          <div className="mt-4 flex gap-2 justify-end">
            <Button variant="ghost" size="sm" onClick={onLater}>
              {t("update.later")}
            </Button>
            <Button size="sm" onClick={onInstall}>
              {t("update.install")}
            </Button>
          </div>
        </DialogPrimitive.Popup>
      </DialogPortal>
    </Dialog>
  );
}
