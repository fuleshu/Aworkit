# Desktop file and folder dialogs

Every user-facing browse in Aworkit goes through the operating system's own
dialog, opened from Rust (`tauri-plugin-dialog`). The webview never receives a
path it did not get from a dialog it did not open.

| Chooser | Surface | Command | Opens for |
| --- | --- | --- | --- |
| Open file | Chat question, extension inspection, MCP command | `native_pick_file` | one file, optionally filtered |
| Choose folder | Chat question, Settings → Projects "Add folder…", new-project dialog | `native_pick_folder` | one directory |
| Add image | Chat composer **+ → Add image** | `native_pick_images` | multiple PNG/JPEG/WebP files |
| Import workflow | Workflow designer → Open | `native_workflow_open_path` | one workflow JSON document |
| Export workflow | Workflow designer → Save / Save As | `native_workflow_save_path` | one workflow JSON document |

## One session, one remembered folder

All five choosers share one process-wide "folder this session last browsed to"
(`crate::dialog_session` in `desktop/src-tauri/src`):

- Every chooser is seeded with the remembered folder, so a browse no longer
  restarts at the application's own installation directory.
- A chosen **file** remembers the folder that contains it; a chosen
  **folder** remembers that folder, which is where the next folder browse
  should open.
- A cancelled dialog remembers nothing, and an empty parent (a bare file name)
  is never stored as a folder.
- The memory is deliberately **not persisted**. It lives for one run of the
  application, exactly like a file manager's own recent-folder behaviour, and it
  is not part of the settings document the user can inspect or edit.
- The remembered folder is a convenience, never an authority: a chooser still
  returns whatever absolute path the user picked, and nothing else can supply a
  path to these commands. The Chat question, Image read and workflow surfaces
  keep their own validation on the returned path.

`native_pick_images` reads and stores the chosen files itself
(`ChatImageStore::import_path`), because the images are imported by the native
runtime that owns image storage; the composer receives stored attachment records
instead of file bytes. The webview `<input type="file">` remains the browser
Preview fallback, where no native chooser exists.
