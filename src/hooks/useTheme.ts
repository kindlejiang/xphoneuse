import { useState, useEffect } from "react";
import type { ThemePreference } from "../types";

function getInitialThemePreference(): ThemePreference {
  const stored = localStorage.getItem("theme");
  if (stored === "light" || stored === "dark" || stored === "auto") return stored;
  return "auto";
}

/** Paint a resolved theme onto the document (the CSS variables hang off it). */
export function applyThemeClass(theme: "light" | "dark") {
  document.documentElement.setAttribute("data-theme", theme);
  document.documentElement.classList.toggle("dark", theme === "dark");
}

/**
 * The stored preference resolved against the OS setting, for windows that
 * only display the theme rather than own it (the settings window paints this
 * before the main window's first snapshot arrives, so it does not flash the
 * wrong one).
 */
export function resolveStoredTheme(): "light" | "dark" {
  const pref = getInitialThemePreference();
  if (pref !== "auto") return pref;
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

export function useTheme() {
  const [themePref, setThemePref] = useState<ThemePreference>(getInitialThemePreference);
  const [systemDark, setSystemDark] = useState(() => window.matchMedia("(prefers-color-scheme: dark)").matches);

  const theme = themePref === "auto" ? (systemDark ? "dark" : "light") : themePref;

  useEffect(() => {
    applyThemeClass(theme);
  }, [theme]);

  useEffect(() => {
    localStorage.setItem("theme", themePref);
  }, [themePref]);

  useEffect(() => {
    if (themePref !== "auto") return;
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const handler = () => setSystemDark(mq.matches);
    mq.addEventListener("change", handler);
    return () => mq.removeEventListener("change", handler);
  }, [themePref]);

  const cycleTheme = () => {
    setThemePref((p) => p === "light" ? "dark" : p === "dark" ? "auto" : "light");
  };

  return { theme, themePref, setThemePref, cycleTheme };
}
