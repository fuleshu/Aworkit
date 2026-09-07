/** Project the UI text role into native menus. The physical monitor scale is
 * applied by Windows; computed CSS pixels already include both text settings. */
export function projectNativeMenuTypography(): void {
  if (!("__TAURI_INTERNALS__" in window)) return;
  const root = document.documentElement;
  const fontSize = parseFloat(getComputedStyle(root).fontSize) * 13 / 14;
  if (!Number.isFinite(fontSize)) return;
  void import("@tauri-apps/api/core").then(({ invoke }) =>
    invoke("native_set_menu_font", { fontSize, dark: root.dataset.appearance === "dark" }),
  ).catch(error => console.warn("Native menu typography is unavailable", error));
}
