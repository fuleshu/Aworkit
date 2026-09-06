# Desktop text scaling

Settings > Appearance > Text size multiplies all application typography, including
Markdown, code, metadata, Settings, notifications and workflow widgets. The saved
value remains relative to the operating-system baseline; preview, discard and reset
do not overwrite OS preferences.

The root preserves the existing compact 13/16 font baseline using a percentage of
the webview's default font. Feature font sizes and line heights use `rem`. Avoid
pixel-sized typography and fixed-height text rows. React Flow's vendor typography
is overridden explicitly; Mantine and native HTML controls inherit the same root.

On Windows, `SystemTextScale` retains a `UISettings` subscription and projects
`TextScaleFactor` before React mounts, then forwards `TextScaleFactorChanged` events.
The frontend subscribes before querying and rejects stale startup snapshots. Windows
text size and Aworkit text size multiply; neither is stored in place of the other.
Monitor DPI remains owned by Tauri/WebView2, so it must not also be multiplied into
CSS. Other platforms retain their native webview's display/default font behavior.

For example, 125% Windows text size and 130% application text size yield 162.5%
of the baseline typography. Display DPI independently maps CSS pixels to the screen.
Windows display scaling and Windows Accessibility > Text size are separate settings.

Platform references: [Windows text-size API](https://devblogs.microsoft.com/oldnewthing/20230830-00/?p=108680),
[text-size change events](https://learn.microsoft.com/en-us/uwp/api/windows.ui.viewmanagement.uisettings.textscalefactorchanged),
[WebView2 monitor scaling](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2controller3).

## Verification

Run `npm test -- src/workbench/systemTextScale.test.ts` from `desktop` for startup,
event ordering, cleanup, preview independence and a fixed-typography regression guard.
Build `desktop/dist` with `npm run build`, then build `desktop/src-tauri/Cargo.toml`.
Run `node scripts/native-text-scale-smoke.mjs` from `desktop` on Windows.

The native test owns a separate profile and local model fixture. It checks every
rendered text element in Chat, Run details, Appearance and Workflows at 100%/130%,
exercises a simulated additional 125% OS event through the actual native event
bridge, and verifies the Settings selector plus save/reload. It records the real OS
text factor and WebView DPI and saves screenshots/reports under `src-tauri/target`.
It does not change global Windows preferences; the simulated event validates the
projection rather than physically moving the Windows text-size slider.
