import { useState, useCallback } from "react";
import type { Toast } from "../types";

let toastId = 0;

export function useToasts() {
  const [toasts, setToasts] = useState<Toast[]>([]);

  // Returns the toast id so long-running work (an update download) can keep
  // updating one toast in place. duration 0 = stays until updated/dismissed.
  const showToast = useCallback(
    (message: string, type: "error" | "info" = "error", duration = 4000) => {
      const id = ++toastId;
      setToasts((prev) => [...prev, { id, message, type }]);
      if (duration > 0) {
        setTimeout(() => setToasts((prev) => prev.filter((t) => t.id !== id)), duration);
      }
      return id;
    },
    [],
  );

  const updateToast = useCallback((id: number, message: string, type?: "error" | "info") => {
    setToasts((prev) => prev.map((t) => (t.id === id ? { ...t, message, type: type ?? t.type } : t)));
  }, []);

  const dismissToast = useCallback((id: number) => {
    setToasts((prev) => prev.filter((t) => t.id !== id));
  }, []);

  return { toasts, showToast, updateToast, dismissToast };
}
