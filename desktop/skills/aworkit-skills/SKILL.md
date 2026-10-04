---
name: aworkit-skills
description: How Aworkit skills work and how to author one - SKILL.md frontmatter, the skill folders Aworkit searches and their precedence, /name and model invocation, invocation controls, and how to test a new skill.
---

# Aworkit skills

A **skill** is a folder of instructions Aworkit lends an agent when a task needs
it. The agent always sees one line per skill - its name and description - and
loads the body only when it decides the skill applies or the user types
`/skill-name`. Every Aworkit skill is plain markdown on disk; nothing is
compiled and nothing is installed.

## 1. The file

`<skill-name>/SKILL.md`, with YAML frontmatter:

```markdown
---
name: pdf-report
description: When to use this skill, in one or two sentences.
---

# Steps

1. ...
```

- `name` (required): lowercase ASCII letters and digits in hyphen-separated
  segments, matching the folder name by convention.
- `description` (required): the only text the agent sees before loading. Write
  it as "use this when ..." and name the words a real request would contain; a
  vague description means the skill never loads.
- `disable-model-invocation: true`: hides the skill from the model catalog; the
  user can still load it with `/name`.
- `user-invocable: false`: `/name` is refused; only the model can load it.
- A root may also hold `<skill-name>.md` instead of a folder. That is the only
  other accepted shape.

The camelCase spellings (`modelInvocable`, `userInvocable`,
`disableModelInvocation`) are rejected.

## 2. Where Aworkit looks

Discovery reads **one level** of every root, in this order, and the first skill
with a given name wins:

1. `.aworkit/skills` in the nearest Git project above the Chat workspace.
2. `.agents/skills` in that same project.
3. Additional skill folders configured in Settings.
4. `~/.aworkit/skills` (global, user-owned; its `.system` folder is skipped).
5. `~/.agents/skills` (global, shared between agent tools).
6. The standard skills bundled with the Aworkit build.

So a project skill always beats a user skill, and both beat a bundled skill of
the same name. A later duplicate is skipped with a `duplicate skill "name"
ignored` warning. Roots are re-read before each agent step, so an edit is picked
up by the next step; there is no watcher and no reload command.

## 3. Invocation

- **Model:** the agent gets a catalog of names and descriptions and calls the
  `skill` tool with an exact name from it. The body arrives in a
  `<skill_content>` block with its base directory.
- **User:** typing `/name` in a message loads that skill immediately. The
  loaded body says not to load it a second time.
- Relative paths written in a skill (for example `references/schema.md`)
  resolve against the skill's own directory - read the reference only when the
  skill says you need it.

## 4. Authoring a skill

1. Pick the layer: a procedure with no typed input and no side effect is a
   skill. Anything that executes, needs arguments or changes the machine is a
   tool - see the `aworkit-plugins` skill.
2. Create the folder in `.aworkit/skills` of the project (or `~/.aworkit/skills`
   to keep it for every project).
3. Keep the body short and put long tables, schemas and examples in
   `references/`, named from the body. Loading a skill costs context; loading a
   reference costs nothing until the agent reads it.
4. State facts that are true of this build; verify anything you assert about
   Aworkit against the code or Settings rather than from memory.
5. Test it: open Settings, run the test for the Skills tool and confirm the
   count includes it, then ask for the request the description should trigger
   and check the skill loads. `desktop/scripts/native-skills-smoke.mjs` is the
   end-to-end check the project itself uses.
