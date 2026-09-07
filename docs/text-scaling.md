# Desktop text scaling

Settings > Appearance > Text size multiplies all application typography, including
Markdown, code, metadata, Settings, notifications and workflow widgets. The saved
value remains relative to the operating-system baseline; preview, discard and reset
do not overwrite OS preferences.

The root uses a 14/16 reading baseline, relative to the webview's default font.
`desktop/src/typography.css` defines five shared roles at 100%:

| Role | CSS pixels | Use |
| --- | --- | --- |
| Caption | 12 | Metadata, timestamps, hints and badges |
| UI | 13 | Navigation, menus, controls and inspector labels |
| Body | 14 | Chat, reasoning, Markdown tables/code and composer text |
| Heading | 15 | Section headings |
| Title | 16 | Surface titles |

Chat previously used 11px and some labels used 8–9px while navigation used 13px.
The narrower 12–16px range makes reading text the baseline and uses weight and
spacing for hierarchy. Feature sizes use role tokens and unitless line heights. Avoid
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

Windows menus keep their native HMENU command routing, accelerators, popup placement
and accessibility names. A window subclass paints their labels with the UI role,
including app and OS text size, and scales the resulting GDI font for the window DPI
exactly once. Owner-drawn items expose `MSAAMENUINFO`; per-item bitmap dimensions
make Windows recalculate the menu bar's width and height after a size change. Fonts
and bitmaps belong to the window and never modify system-wide settings. High contrast
uses Windows colors. Other platforms retain their standard native menu rendering.

Platform references: [Windows text-size API](https://devblogs.microsoft.com/oldnewthing/20230830-00/?p=108680),
[text-size change events](https://learn.microsoft.com/en-us/uwp/api/windows.ui.viewmanagement.uisettings.textscalefactorchanged),
[WebView2 monitor scaling](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2controller3).

## Verification

Run `npm test -- src/workbench/systemTextScale.test.ts src/workbench/nativeMenuTypography.test.ts` from `desktop` for startup,
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
It also checks the 12–16px baseline, the 14px chat body, and native menu font/width/
height changes during Settings preview and save/reload. Set `AWORKIT_QA_KEEP_OPEN=1`
to leave the isolated app available for native menu visual and keyboard checks.
