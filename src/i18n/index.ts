import i18next from "i18next";
import { initReactI18next } from "react-i18next";
import en from "./en";
import zh from "./zh";

export const APP_LANG_KEY = "xphoneuse_lang";
export const APP_LANGUAGES = [
  { code: "en", label: "English" },
  { code: "zh", label: "简体中文" },
] as const;
export type AppLanguage = (typeof APP_LANGUAGES)[number]["code"];

function getInitialLanguage(): AppLanguage {
  const stored = localStorage.getItem(APP_LANG_KEY);
  if (stored === "en" || stored === "zh") return stored;
  return (navigator.language || "").toLowerCase().startsWith("zh") ? "zh" : "en";
}

void i18next.use(initReactI18next).init({
  resources: {
    en: { translation: en },
    zh: { translation: zh },
  },
  lng: getInitialLanguage(),
  fallbackLng: "en",
  interpolation: { escapeValue: false },
});

/** Switch language and persist the choice. */
export function setAppLanguage(lang: AppLanguage) {
  localStorage.setItem(APP_LANG_KEY, lang);
  void i18next.changeLanguage(lang);
}

/**
 * Translate outside React components (event callbacks inside hooks fire
 * after render; this reads the live language at call time).
 */
export function t(key: string, options?: Record<string, unknown>): string {
  return i18next.t(key, options) as string;
}

export default i18next;
