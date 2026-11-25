---
name: davinci-resolve
description: Working with DaVinci Resolve Studio through its native MCP server in Aworkit - checking the connection, discovering the scripting API before writing a script, reading a project or grade, rendering frames for review, and authoring LUTs and DCTLs.
---

# Driving DaVinci Resolve through its MCP server

The server exposes a small general surface, not one tool per Resolve feature. Almost all work
goes through `run_script` after you have read the *installed* API.

## 1. Confirm the session before touching anything

`get_resolve_status` returns `{"running": true, "version": "..."}`. If it reports not running,
`launch_resolve` starts the application and waits up to 60 seconds; then read the state again.

Report what you found — version, product, open project, current page — instead of assuming it.
The current version may be newer than your training data: `get_whats_new` with `since` set to the
last version you know is cheap and prevents planning a workflow a newer release has replaced.

## 2. Discover, never guess

Before writing a `run_script` body:

1. `search_scripting_api` with a topic regex (`"marker"`, `"GetSetting"`, `"AppendToTimeline"`).
2. Widen with `get_scripting_api` `outline=true`, or `types=["Timeline"]` for one definition.
3. Only fetch a whole stub with `as_file=true` when you can grep the returned path locally.

`get_scripting_docs` covers what the stubs leave out (settings dictionaries, enumerated values,
deprecations). Ask for `section: "TOC"` first; `FusionToolReference.md` is 1.3 MiB and should
only ever be read by section or file.

## 3. Script shape

`resolve` and `project` are already in scope, and `result` is what comes back:

```python
timeline = project.GetCurrentTimeline()
result = {
    "project": project.GetName(),
    "timeline": timeline.GetName() if timeline else None,
    "start_frame": timeline.GetStartFrame() if timeline else None,
    "end_frame": timeline.GetEndFrame() if timeline else None,
}
```

- Return a small, JSON-serialisable dict. Never return a Resolve object.
- Keep the default 10 s timeout in mind; raise it only for a genuinely long pass, never above 60.
- `run_script` is sandboxed away from the filesystem, network and subprocesses. If a task needs a
  file, use an Aworkit file tool; don't reach for `run_script_unsafe` to read a path.

## 4. Read → change → re-read

The scripting API reports success generously. For any edit:

1. Read the current state (timeline items, track count, clip names, ranges) and say what you found.
2. Make one small, named change.
3. Re-read and confirm the intended clips, ranges or tracks actually changed.

Do not stack a batch of edits on an unverified first attempt, and never move the playhead or
change the current timeline merely to inspect something — the user is looking at that view.

## 5. Looking at pictures

- `describe_grade` answers *what the grade contains*: nodes, tools, LUTs, CDLs. Use it instead of
  rendering frames to compare by eye.
- `get_image` answers *what it looks like*. `source: "media_pool_clip"` is the ungraded before
  reference; `source: "timeline"` is the graded composite. The playhead is never moved, so pass
  `timecode` to pick a frame.
- Crop with `roi_*` fractions rather than raising `max_dimension`: a region is rendered at
  `max_dimension`, so cropping gains real detail.
- `get_thumbnails` gives one labelled grid for a range — cheaper than repeated `get_image` calls
  when the question spans a shot or the whole timeline.

## 6. LUTs and DCTLs

Both live under the Resolve support `LUT` folder; the MCP subfolder is where new files are
written. `list_luts` first, `read_dctl` before editing an existing DCTL, `get_scripting_docs`
document `DCTLReadme.txt` before authoring one. `generate_lut` evaluates your Python transform
over a lattice, so keep the transform simple and monotonic and say which `size` you chose.

Only delete a LUT or DCTL the user named, or one you created in this session.

## 7. Missing AI packages

Some scripting calls need optional Extras Download Manager packages. `list_extras_packages`
reports what is available and installed; `install_extras_package` starts a large download, so ask
the user before queueing one and poll `list_extras_packages` for progress.

## 8. When something is wrong

- Not running → `launch_resolve`, then wait before inspecting a project.
- Running but unreachable → ask the user to check
  **Preferences → System → General → External scripting using: Local**, then rediscover the server.
- A method missing from the stubs → the installed API is authoritative; search again rather than
  using a signature from memory.
