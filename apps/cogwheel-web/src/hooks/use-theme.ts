import React from "react";
import { THEME_STORAGE_KEY } from "@/lib/constants";

export type ThemePreference = "light" | "dark" | "system";
export type ResolvedTheme = "light" | "dark";

function readPreference(): ThemePreference {
  try {
    const stored = window.localStorage.getItem(THEME_STORAGE_KEY);
    if (stored === "light" || stored === "dark" || stored === "system") return stored;
  } catch {
    /* localStorage can throw in private mode; the default is fine. */
  }
  return "system";
}

function systemTheme(): ResolvedTheme {
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

/**
 * Applies the theme by mutating <html> directly rather than re-rendering a
 * provider tree. The inline script in index.html does the same thing before
 * first paint, so this only ever has to handle *changes*.
 */
function apply(resolved: ResolvedTheme): void {
  const root = document.documentElement;
  root.setAttribute("data-theme", resolved);
  root.classList.toggle("dark", resolved === "dark");
  root.style.colorScheme = resolved;
}

export function useTheme() {
  const [preference, setPreference] = React.useState<ThemePreference>(readPreference);
  const [resolved, setResolved] = React.useState<ResolvedTheme>(() =>
    readPreference() === "system" ? systemTheme() : (readPreference() as ResolvedTheme),
  );

  React.useEffect(() => {
    const next = preference === "system" ? systemTheme() : preference;
    setResolved(next);
    apply(next);

    try {
      window.localStorage.setItem(THEME_STORAGE_KEY, preference);
    } catch {
      /* Persistence is best-effort. */
    }

    if (preference !== "system") return;

    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = () => {
      const followed = systemTheme();
      setResolved(followed);
      apply(followed);
    };
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  }, [preference]);

  return { preference, resolved, setPreference };
}
