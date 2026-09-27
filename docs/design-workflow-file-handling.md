# Workflow designer file handling

The Workflow designer edits one workflow document. It handles that document the
way a desktop editor handles a file — New, Open, Save, and Save As — while the
native workflow library stays the one canonical store. This note records the
resulting surface and the rules behind it.

## Surface

The toolbar offers, in order: `New`, `Open`, `Save` (primary), `Save As`,
`↶ Undo`, `↷ Redo`, `Validate <count>`. There is no `Import JSON`, no `Export`,
and no `Run` control on this surface: importing a document locally and exporting
it as a browser download were the defect being replaced, and starting a Run
belongs to the Chat composer.

The saved-workflow strip keeps the workflow selector, `New from`, `Name`,
`Create`, `Duplicate`, `Delete`, and `Set default`. `Rename` is gone: the
workflow's own Name property is the display name, and a save that changed it
renames the stored entry. The strip's Name field only names a `Create` or
`Duplicate` target.

## File commands

- **New** starts the bundled creation-default (`Blank`) template as an unsaved
  draft. The draft has no bound file, so its first `Save` asks for a path, and
  nothing is stored until the core accepts a save.
- **Open** asks the operating system for a file, parses it with the existing
  lossless parser, validates it, activates it, and immediately commits the
  activated document through the same core-accepted save the library uses. That
  commit is what makes a loaded workflow survive switching to another workflow
  and back. The opened file becomes the file `Save` writes.
- **Save** writes the complete JSON document to the currently bound file and
  commits it. With no file bound it behaves exactly like Save As.
- **Save As** asks for a path, writes the document, and binds that path for
  later saves. If the chosen path already holds a file, the user is asked before
  it is replaced.

Open and Save As choose only a path passed through the operating system's own
dialogs. There is no hidden `<input type="file">` and no Blob download. Browser
Preview has neither a file chooser nor files to write, so every file operation
there reports that it needs the native desktop runtime instead of pretending to
have performed one.

## Identity and naming

A workflow library entry is the canonical identity of a document. A file is a
path bound to the entry the editor is showing, so the binding is remembered per
entry and switching workflows can never write one entry's document over another
entry's file. A document loaded from a file (or from a template) is rebound to
the identity of the entry it is activated in before it is stored, because the
core requires a stored document to keep its library identity; everything else in
the loaded document — nodes, transitions, positions, requirement metadata, and
unknown fields — is kept verbatim. Deleting an entry, or opening a document into
a read-only (future schema) entry, is refused rather than silently rewritten.

A file name and a workflow name may differ, and nothing may depend on them
matching:

- the Name property (and a `name` stored inside a file) is the workflow name;
- Open and Save As choose a path only;
- a `name` stored inside the file wins on load, and the file basename is used
  only as a suggestion for a document that carries no name;
- Save As offers the workflow name as its file-name suggestion, and what the
  user types there wins.

## Unsaved changes, invalid files, and the library

`New` and `Open` ask about unsaved changes through the application's own native
confirmation dialog before they discard them; declining keeps the draft and does
not even open a file chooser. The library selector is unchanged: it reloads the
stored entry of whatever workflow is selected, so a draft is saved before
switching workflows.

An invalid or corrupt file changes nothing. A file that cannot be read, is not
UTF-8 text, does not parse as a workflow JSON document, or carries a document
the workflow library would refuse (an unsupported schema version, or structural
validation errors) reports a clear error naming the file and the reason, and
leaves editor state and the workflow library exactly as they were. There is no
partially applied document. Unresolved requirements are still not a reason to
refuse a file: a missing dependency stays visible and editable, and it gates
execution only.

File operations never install, enable, or grant anything: Open and Save only
read and write the document the user chose.

## Native boundary

`desktop/src-tauri/src/workflow_files.rs` owns the host side of these commands:
the open and save dialogs, a bounded UTF-8 read, and an atomic write that refuses
to replace an existing file unless its caller confirmed the replacement. A
workflow file is read and written as one document of at most 4 MiB; a larger
file, a directory, a missing file, and non-UTF-8 bytes are each their own error.
Writes go to a temporary file beside the target and are renamed over it, so an
interrupted write cannot leave half a workflow behind, and a refused write leaves
the existing document byte-for-byte untouched.

Replacing a file is therefore a two-step protocol. The designer first writes
with `overwrite: false`; the host answers `exists` instead of writing, the
designer asks the user, and only a confirmed answer writes again with
`overwrite: true`. A webview that forgot to ask cannot destroy a file. `main.rs`
exposes `native_workflow_open_path`, `native_workflow_save_path`,
`native_workflow_read_file`, and `native_workflow_write_file` as thin wrappers
over that module, and the confirmation dialogs reuse the existing
`native_confirm` message dialog.

The TypeScript ports mirror that boundary: `WorkflowFilePort` in
`desktop/src/workbench/workflowFilePort.ts` has a Tauri implementation and an
honest Preview implementation, and the editor consumes only the port, so tests
drive Open and Save As through an injected double with no real dialog.

## Out of scope

Browse-for-file import of run history, exporting a shareable bundle, and
multi-document tabs are not part of this surface. `Run` and `Export` remain
absent by decision: a workflow reaches a Chat through the composer, and the file
commands above already write the complete lossless document.
