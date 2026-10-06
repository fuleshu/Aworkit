# Tool plugins and skills

Aworkit extensions come in two deliberately separate layers: **skills** carry
knowledge, and **tool plugins** package a server that adds capabilities. A
single plugin folder can bundle an MCP server *and* one or more skills.

| Layer | What it is | What it adds |
| --- | --- | --- |
| **Skill** | A folder with a `SKILL.md` file | Knowledge only |
| **MCP tool** | A capability with a typed input, a result and possibly a side effect | An action the model can propose |
| **Plugin** | The installable, versioned package that owns the MCP server and optional skills | The delivery and trust unit |

## Tool plugins

One plugin is one folder containing a `tool-plugin.json` declaration plus
whatever the server needs (a script or binary, a README and optional
`skills/<name>/SKILL.md` folders). The declaration states the identity — a stable
`id`, a display name and a version — and exactly one MCP transport:

- **stdio** — a local command plus arguments, with an optional working directory
  and environment credential bindings. A bare command name (`python`, `node`)
  resolves from `PATH`; a relative command or directory resolves inside the
  plugin folder and can never escape it.
- **streamable HTTP** — a server URL with optional header credential bindings.

An optional `tools[]` array seeds names, descriptions, input schemas and
per-tool instructions before the first connection. The server's live catalog
stays authoritative, and refreshing discovery preserves your own instructions
and approval choices.

### Install and trust

Open **Settings → Tool Plugins**:

- **Open plugin folder** shows where Aworkit keeps plugins.
- **Install plugin…** copies a folder you choose into that plugin folder.
- **Refresh** re-reads the folder; it is a read and runs no code.
- **Add plugin**, then **Connect and enable**, then **Save configuration**.

Copying a folder in only *sources* it: the plugin is listed, turned off, and
nothing runs until you add and enable it. That is the explicit trust act.
Installation pins the declaration path, content hash and version, so a changed
package is reported and must be accepted again. Removing a plugin deletes the
folder; a workflow that referenced it reports a missing capability and keeps
running with what is present.

Secrets are never embedded in the declaration. `env` and `headers` name stored
credential fields resolved only for the connection.

## Skills

A skill is plain Markdown on disk: `<name>/SKILL.md` with `name` and
`description` frontmatter. The agent always sees one line per skill and loads the
body only when it decides the skill applies, or when you type `/skill-name`.

Discovery reads one level of every root, and the first skill with a given name
wins:

1. `.aworkit/skills` in the project
2. `.agents/skills` in the project
3. additional folders configured in Settings
4. `~/.aworkit/skills` (global)
5. `~/.agents/skills` (global, shared between agent tools)
6. the standard skills bundled with the Aworkit build

So a project skill always beats a user or bundled one. `disable-model-invocation:
true` hides a skill from the model catalog (it can still be loaded with
`/skill-name`), and `user-invocable: false` refuses `/skill-name`.

### Bundled standard skills

Every build ships skills that teach the agent how Aworkit itself works:
`aworkit-workflows`, `aworkit-skills`, `aworkit-plugins`, `aworkit-chat-storage`
and the `plugin-authoring` guide. They sit at the lowest priority, so your own
skill of the same name wins and an upgrade replaces them without touching
anything you wrote.

## See also

- The reference plugin: [Example workflows and the FFmpeg plugin](optional-extras.md)
- The deeper design notes: [Further reading](further-reading.md)
