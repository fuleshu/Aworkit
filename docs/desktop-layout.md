# Desktop layout persistence

The global settings JSON owns `layout`. The native host captures the window;
the shell reports separator changes when a drag finishes or the keyboard moves
a separator. Hydration and renderer unload do not write default pane widths.

## Coordinate contract

- `x` and `y` describe the **outer** frame origin in physical pixels.
- `width` and `height` are the size the restore applies, in physical pixels:
  the Win32 outer rectangle on Windows, the client size everywhere else.
  Capture and restore are a bijection so the frame never drifts between runs.
- `scaleFactor` records the display scale; already physical bounds are not scaled again.
- `historyPaneWidth` and `inspectorPaneWidth` are integer CSS pixels. Fractional
  pointer coordinates are rounded before IPC, since the native schema uses `u32`.
- `maximized` retains maximized mode separately from the last normal frame.
  Minimization never records the Windows sentinel coordinates.

On Windows, restoration uses `SetWindowPos` with the outer rectangle. Tauri's
`set_size` takes a client size and adds decorations; passing saved outer bounds
to it grows the frame on every restart. The native custom menu also affects
the non-client area. On the other platforms the captured client size is applied
to `set_size` directly: the decoration is never re-derived from the live window,
because on Linux `tao` refreshes its insets only from `_NET_FRAME_EXTENTS`, which
is empty for client-side decorations — subtracting that read back used to grow
the window by its decoration on every restart (or collapse it when the reported
inset degenerated). A saved position whose title bar is unreachable on current
monitors is skipped.

A measurement that would not survive `validate()` (a zero size read before the
window is realized, an impossible scale factor, an out-of-range coordinate) is
discarded rather than stored, so one bad sample cannot block the final write and
leave an older frame on disk.

A record written by an earlier build holds an outer size on the non-Windows
platforms. The first restore after this change applies it as a client size once
(a frame that is smaller by the decoration, and possibly already wrong from the
inset read it was captured with); the next close stores the client size, so the
round trip is exact from then on.

Position is best-effort on Linux, and on a Wayland session it is not attempted
at all: `set_position` runs after Tauri has mapped the window, which X11 window
managers usually honour but tiling managers may not, while a Wayland compositor
owns placement outright — `xdg-shell` has no position request,
`gtk_window_move` does nothing, and `gtk_window_get_position` always reports
`(0, 0)`. A Wayland session therefore stores no `x`/`y` (a read-back would only
ever be that meaningless origin, which would also misplace the window if the
same profile were later opened on X11) and never asks the compositor to move
the window. The size always applies on every platform. Running the app as an
X11 client (`GDK_BACKEND=x11`, i.e. through XWayland) restores the position
again, because that session can place its own windows.

`desktop_layout.rs` retains current measurements independently of the runtime
coordinator. Native Close and File > Quit wait for a worker to acquire the
coordinator and save the latest layout before destroying the window. Older
queued separator writes read the latest session layout under that coordinator,
so they cannot overwrite newer measurements. No renderer unload IPC is required.
Windows maximize detection uses `IsZoomed` because Tao's cached flag can lag
native move/resize callbacks.

## Verification

Build the frontend from `desktop` with `npm run build`, then build the host:

```powershell
cargo build --manifest-path desktop/src-tauri/Cargo.toml --bin aworkit-desktop
```

From `desktop`, run `npm run test:native-layout`. It copies the debug binary,
uses an isolated profile under `src-tauri/target/native-layout-*`, drives the
real WebView2 separators, and uses Win32 to place and close only its own window.
It checks repeated restarts, fractional drags, outer/client dimensions, native
File > Quit, maximized/unmaximized bounds, minimized closes, and reachable
negative coordinates. The JSON report contains the measurements. The regression
was reproduced at 125% display scaling: 1320 x 820 became 1338 x 892, and
249.5/355.5 separators reopened as 208/320 before the fix.
