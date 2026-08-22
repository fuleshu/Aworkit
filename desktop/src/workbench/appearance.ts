/** Canonical compact-desktop appearance roles shared by every feature surface. */
export type AppearancePreference = "system" | "light" | "dark";
export type ResolvedAppearance = Exclude<AppearancePreference, "system">;
let projectedAppearance: AppearancePreference = "system";

export interface AppearanceEnvironment {
  readonly prefersDark: boolean;
  readonly forcedColors: boolean;
  readonly reducedMotion: boolean;
  readonly fontScale?: number;
}

export const lightTokens = {
  window: "#F7F8FA",
  panel: "#FFFFFF",
  navigation: "#F1F3F5",
  raised: "#FAFAFB",
  inset: "#EEF1F4",
  divider: "#D8DDE3",
  control: "#8B95A2",
  text: "#1E2228",
  secondary: "#5B6470",
  muted: "#626B76",
  accent: "#315FD6",
  accentSubtle: "#EAF0FF",
  selected: "#E5ECFA",
  onAccent: "#FFFFFF",
  info: "#2653A3",
  infoSurface: "#EBF1FF",
  success: "#176B43",
  successSurface: "#EAF7F0",
  warning: "#7A4300",
  warningSurface: "#FFF3D6",
  danger: "#A52A22",
  dangerSurface: "#FDEDEC",
  opaque: "#4F5866",
  opaqueSurface: "#EFF1F4",
} as const;
export const darkTokens: Record<keyof typeof lightTokens, string> = {
  window: "#111318",
  panel: "#17191D",
  navigation: "#15181D",
  raised: "#1D2128",
  inset: "#0D0F13",
  divider: "#2E343D",
  control: "#5E6876",
  text: "#F1F3F5",
  secondary: "#B7BDC7",
  muted: "#929AA6",
  accent: "#8AA8FF",
  accentSubtle: "#1B2742",
  selected: "#202A40",
  onAccent: "#111827",
  info: "#9BB5FF",
  infoSurface: "#17233A",
  success: "#79D39E",
  successSurface: "#14271D",
  warning: "#F2C36E",
  warningSurface: "#2B2111",
  danger: "#FF9B92",
  dangerSurface: "#321817",
  opaque: "#BCC2CC",
  opaqueSurface: "#242830",
};

export function resolveAppearance(
  preference: AppearancePreference,
  environment: AppearanceEnvironment,
): ResolvedAppearance {
  return preference === "system"
    ? environment.prefersDark
      ? "dark"
      : "light"
    : preference;
}

/** Applies tokens before React mounts; System can later follow OS color-scheme changes. */
export function applyAppearance(
  root: HTMLElement,
  preference: AppearancePreference,
  environment: AppearanceEnvironment,
): ResolvedAppearance {
  const resolved = resolveAppearance(preference, environment);
  const tokens = resolved === "dark" ? darkTokens : lightTokens;
  for (const [name, value] of Object.entries(tokens))
    root.style.setProperty(`--aw-${kebab(name)}`, value);
  root.style.setProperty("--aw-font-scale", String(environment.fontScale ?? 1));
  root.dataset.appearance = resolved;
  root.dataset.forcedColors = String(environment.forcedColors);
  root.dataset.reducedMotion = String(environment.reducedMotion);
  return resolved;
}

export function browserAppearanceEnvironment(): AppearanceEnvironment {
  const query = (value: string) => window.matchMedia?.(value).matches ?? false;
  return {
    prefersDark: query("(prefers-color-scheme: dark)"),
    forcedColors: query("(forced-colors: active)"),
    reducedMotion: query("(prefers-reduced-motion: reduce)"),
    fontScale: 1,
  };
}

/** Applies a core-projected or local-preview preference without web storage. */
export function projectAppearancePreference(
  preference: AppearancePreference,
): ResolvedAppearance {
  projectedAppearance = preference;
  return applyAppearance(
    document.documentElement,
    preference,
    browserAppearanceEnvironment(),
  );
}

/** Keeps System appearance synchronized without overriding an explicit saved choice. */
export function initializeBrowserAppearance(): () => void {
  const media = window.matchMedia("(prefers-color-scheme: dark)");
  const applyCurrent = () => {
    if (projectedAppearance === "system")
      applyAppearance(
        document.documentElement,
        projectedAppearance,
        browserAppearanceEnvironment(),
      );
  };
  applyCurrent();
  media.addEventListener?.("change", applyCurrent);
  return () => media.removeEventListener?.("change", applyCurrent);
}

/** WCAG relative contrast ratio used by the token-matrix QA gate. */
export function contrastRatio(foreground: string, background: string): number {
  const [bright, dark] = [luminance(foreground), luminance(background)].sort(
    (a, b) => b - a,
  );
  return (bright + 0.05) / (dark + 0.05);
}
function luminance(hex: string): number {
  const values = hex.match(/[A-Fa-f0-9]{2}/g);
  if (values === null || values.length !== 3)
    throw new Error(`invalid hex color ${hex}`);
  return values
    .map((part) => Number.parseInt(part, 16) / 255)
    .map((value) =>
      value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4,
    )
    .reduce(
      (sum, value, index) => sum + value * [0.2126, 0.7152, 0.0722][index],
      0,
    );
}
function kebab(value: string): string {
  return value.replace(/[A-Z]/g, (character) => `-${character.toLowerCase()}`);
}
