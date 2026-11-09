import { useTranslation } from "react-i18next";
import { Dialog, DialogPortal, DialogOverlay, DialogTitle } from "@/components/ui/dialog";
import { Dialog as DialogPrimitive } from "@base-ui/react/dialog";
import { Button } from "@/components/ui/button";
import { isPasscodeRefusal, type IosDevModePrompt } from "../hooks/useConnection";

/**
 * The "this iPhone needs Developer Mode" conversation.
 *
 * The switch is what installs the developer image the screen, touch and
 * screenshots come from, and turning it on restarts the phone - so the app
 * never does it silently: this counts down first (and can be stopped), then
 * waits for the phone to come back before letting the user continue into the
 * mirror. It is also where the app admits it could not do it, which is the
 * only case that asks the user to go and flip it by hand.
 */
export function IosDevModeDialog({
  prompt,
  devicePresent,
  onCancel,
  onEnableNow,
  onContinue,
}: {
  prompt: IosDevModePrompt | null;
  /** Whether the phone is visible to us again after its restart. */
  devicePresent: boolean;
  onCancel: () => void;
  onEnableNow: () => void;
  /** Connect again - the phone is back, or the user flipped the switch. */
  onContinue: () => void;
}) {
  const { t } = useTranslation();
  if (!prompt) return null;

  const title =
    prompt.stage === "waiting"
      ? t("iosDevMode.waitingTitle")
      : prompt.stage === "manual"
        ? t("iosDevMode.manualTitle")
        : t("iosDevMode.title");

  return (
    <Dialog open onOpenChange={(open) => !open && onCancel()}>
      <DialogPortal>
        <DialogOverlay />
        <DialogPrimitive.Popup className="fixed left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 w-[min(92vw,360px)] bg-surface border border-border rounded-2xl shadow-xl z-51 p-5 animate-in fade-in zoom-in-[0.98] duration-200">
          <DialogTitle className="text-[15px] font-bold text-foreground">{title}</DialogTitle>

          {prompt.stage === "ask" && (
            <>
              <p className="mt-2 text-[13px] leading-relaxed text-muted-foreground">
                {t("iosDevMode.askBody", { seconds: prompt.seconds ?? 0 })}
              </p>
              <p className="mt-2 text-xs text-muted-foreground">{t("iosDevMode.askHint")}</p>
              <div className="mt-4 flex gap-2 justify-end">
                <Button variant="ghost" size="sm" onClick={onCancel}>
                  {t("iosDevMode.cancel")}
                </Button>
                <Button size="sm" onClick={onEnableNow}>
                  {t("iosDevMode.enableNow")}
                </Button>
              </div>
            </>
          )}

          {prompt.stage === "enabling" && (
            <p className="mt-2 text-[13px] leading-relaxed text-muted-foreground">
              {t("iosDevMode.enabling")}
            </p>
          )}

          {prompt.stage === "waiting" && (
            <>
              <p className="mt-2 text-[13px] leading-relaxed text-muted-foreground">
                {t("iosDevMode.waitingBody")}
              </p>
              <p className="mt-2 text-xs text-muted-foreground">
                {devicePresent ? t("iosDevMode.deviceBack") : t("iosDevMode.waitingDevice")}
              </p>
              <div className="mt-4 flex gap-2 justify-end">
                <Button variant="ghost" size="sm" onClick={onCancel}>
                  {t("iosDevMode.cancel")}
                </Button>
                <Button size="sm" onClick={onContinue} disabled={!devicePresent}>
                  {t("iosDevMode.continue")}
                </Button>
              </div>
            </>
          )}

          {prompt.stage === "manual" && (
            <>
              <p className="mt-2 text-[13px] leading-relaxed text-muted-foreground">
                {/* A passcode refusal is the device's own rule, not a failure
                    of ours: iOS 27 will not take the request from the host,
                    while the switch itself works fine by hand. */}
                {isPasscodeRefusal(prompt.error)
                  ? t("iosDevMode.passcodeBody")
                  : t("iosDevMode.manualBody")}
              </p>
              {prompt.error && (
                <p className="mt-2 text-xs text-muted-foreground break-words">{prompt.error}</p>
              )}
              <div className="mt-4 flex gap-2 justify-end">
                <Button variant="ghost" size="sm" onClick={onCancel}>
                  {t("iosDevMode.cancel")}
                </Button>
                <Button size="sm" onClick={onContinue}>
                  {t("iosDevMode.retry")}
                </Button>
              </div>
            </>
          )}
        </DialogPrimitive.Popup>
      </DialogPortal>
    </Dialog>
  );
}
