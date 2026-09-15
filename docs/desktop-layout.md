# Desktop layout persistence

The global settings JSON owns `layout`. The native host captures the window;
the shell reports separator changes when a drag finishes or the keyboard moves
a separator. Hydration and renderer unload do not write default pane widths.

## Coordinate contract

- `x`, `y`, `width`, and `height` describe the **outer** frame in physical pixels.
- `scaleFactor` records the display scale; already physical bounds are not scaled again.
- `historyPaneWidth` and `inspectorPaneWidth` are integer CSS pixels. Fractional
  pointer coordinates are rounded before IPC, since the native schema uses `u32`.
- `maximized` retains maximized mode separately from the last normal frame.
  Minimization never records the Windows sentinel coordinates.

On Windows, restoration uses `SetWindowPos` with the outer rectangle. Tauri's
`set_size` takes a client size and adds decorations; passing saved outer bounds
to it grows the frame on every restart. The native custom menu also affects
the non-client area. Other platforms subtract measured native frame insets
before calling the client-size setter. A saved position whose title bar is
unreachable on current monitors is skipped.

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
