/** Appearance resolution happens before rendering so a stored theme never flashes. */
export type AppearancePreference = "system" | "light" | "dark";
export type ResolvedAppearance = Exclude<AppearancePreference, "system">;

export interface AppearanceEnvironment {
  readonly prefersDark: boolean;
  readonly forcedColors: boolean;
  readonly reducedMotion: boolean;
}

export const lightTokens = {
  canvas: "#f5f7fa", surface: "#ffffff", raised: "#f8fafc", text: "#182230",
  muted: "#526071", border: "#d8dee8", accent: "#356ae6", focus: "#1d4ed8",
  danger: "#b42318", success: "#067647",
} as const;
export const darkTokens = {
  canvas: "#14181f", surface: "#1c232d", raised: "#252d38", text: "#edf2f7",
  muted: "#aab7c8", border: "#384454", accent: "#8ab4ff", focus: "#b6d0ff",
  danger: "#ffb4ab", success: "#72e0a2",
} as const;

/** Resolves the persisted preference without consulting widget-library state. */
export function resolveAppearance(preference: AppearancePreference, environment: AppearanceEnvironment): ResolvedAppearance {
  return preference === "system" ? (environment.prefersDark ? "dark" : "light") : preference;
}

/** Applies semantic CSS variables once to the workbench document root. */
export function applyAppearance(root: HTMLElement, preference: AppearancePreference, environment: AppearanceEnvironment): ResolvedAppearance {
  const resolved = resolveAppearance(preference, environment);
  const tokens = resolved === "dark" ? darkTokens : lightTokens;
  for (const [name, value] of Object.entries(tokens)) root.style.setProperty(`--aw-${name}`, value);
  root.dataset.appearance = resolved;
  root.dataset.forcedColors = String(environment.forcedColors);
  root.dataset.reducedMotion = String(environment.reducedMotion);
  return resolved;
}

export function browserAppearanceEnvironment(): AppearanceEnvironment {
  const query = (value: string) => window.matchMedia?.(value).matches ?? false;
  return { prefersDark: query("(prefers-color-scheme: dark)"), forcedColors: query("(forced-colors: active)"), reducedMotion: query("(prefers-reduced-motion: reduce)") };
}
