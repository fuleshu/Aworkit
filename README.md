# Aworkit

**Agent Workflow Toolkit** — a free, open-source desktop app where you design AI-agent workflows visually and run them in a familiar chat window, with full visibility of everything that happens.

🌐 **Website: [klutzgames.com/aworkit](https://www.klutzgames.com/aworkit/)**

![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)
![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20Linux-lightgrey.svg)
![Status](https://img.shields.io/badge/status-v0.1.0%20early%20development-orange.svg)

---

## Table of contents

- [What is Aworkit?](#what-is-aworkit)
- [Features](#features)
- [Installation](#installation)
  - [Installer from GitHub](#installer-from-github)
  - [Build from source — Windows](#build-from-source--windows)
  - [Build from source — Linux](#build-from-source--linux)
- [Development mode](#development-mode)
- [Built with Adashi](#built-with-adashi)
- [Libraries and licenses](#libraries-and-licenses)
- [License](#license)
- [Contributing](#contributing)

---

## What is Aworkit?

**The elevator pitch:** most AI chat apps only show you a conversation. Aworkit shows you the *machine* behind it. It is a desktop application where every chat is actually a complete, visible agent workflow — you can open it, see how it works, and reshape it with a mouse.

**In more detail:**

- **A chat is a workflow.** When you start a chat, you pick a workflow — a graph of steps such as *model call → tool → condition → approval → answer*. The workflow decides how your request is processed. A simple chat is just a few steps; an advanced one can branch, loop, call tools, and delegate to sub-agents.
- **You design workflows visually.** Workflows live as plain, portable JSON documents that you edit on a drag-and-drop canvas. No hidden configuration, no lock-in — you can export them, diff them, and share them.
- **Your models, your choice.** Aworkit is model-location-neutral: a local model, a hosted API (OpenAI-compatible or others), or external coding agents like Codex are all equal citizens. Run it fully offline, fully hosted, or any mix in between.
- **Everything stays on your machine.** Aworkit is a desktop application, not a web service. Your chats, workflows, and settings are stored locally; your credentials go into the operating system's credential store.
- **Nothing is hidden.** Every model call, tool action, approval, and error is recorded and inspectable — with token usage, timings, and the raw data behind each step.

Aworkit is at an **early stage (v0.1.0)**. The core loop works, but expect rough edges and breaking changes.

## Features

### 💬 Chat that runs real workflows

A familiar chat interface — but each chat runs one selected workflow from start to finish. The sidebar organises your projects and chat history, the composer lets you pick the workflow, project, and approval mode, and the **Run details** panel on the right shows live token usage, cache hit rate, and a complete execution log of every model call, tool call, and sub-agent.

![Aworkit chat with run details](screenshots/chat.png)

### 🧩 Visual workflow editor

Drag nodes onto a canvas, connect them, and configure them — no code required. The built-in node types cover everything a real agent needs: **Chat Input, Model Call, Agent, Tool, External Agent, Condition, Bounded loop, Parallel, Approval, Chat Output, Wait for Input,** and **Completion**. The properties panel on the right configures the selected node (model tier, reasoning effort, enabled tools, …).

A simple linear workflow (one agent that answers and waits for your next message):

![Standard agent workflow in the visual editor](screenshots/workflow%20standard.png)

A branching workflow: the request is classified first, then routed — simple questions get a direct answer, everything else goes through an approval gate before a grounded answer is produced:

![Triage router workflow with a condition branch](screenshots/workflow%20triage.png)

### ⚙️ Everything configured in one place

Settings cover **model providers and credentials, model tiers, built-in tools, extensions, MCP servers, external agents, projects, data retention, and appearance** (light/dark, font size). Workflows only reference models and tools by portable logical names — so you can swap a provider or model without touching your workflows.

![Aworkit settings: providers and models](screenshots/settings.png)

### 🛠️ Built-in tools and integrations

- **File tools** — read, search, edit, and write files in your project workspace
- **Shell and Python** — run commands and scripts on your machine
- **Web tools** — search the web, fetch and extract page content
- **Sub-agents** — delegate parts of a task to parallel helper agents
- **MCP servers** — connect any [Model Context Protocol](https://modelcontextprotocol.io) server and use its tools inside your workflows
- **External agents** — plug in lifecycle-owning agents such as Codex or Claude Code

### 🔐 Approvals and transparency

Actions that write files, run commands, or spend money can require your explicit approval — per chat and per step. And when something happens, you can always look behind the curtain: expand any step to see its exact input, output, and raw JSON.

## Installation

### Installer from GitHub

Prebuilt installers are published on the **[Releases page](https://github.com/fuleshu/Aworkit/releases)**:

| Platform | Package |
| --- | --- |
| Windows | `.msi` installer or Windows setup `.exe` |
| Linux (`.deb`) | Debian / Ubuntu package |
| Linux (`.rpm`) | Fedora / openSUSE package |
| Linux (`.AppImage`) | runs on most distributions |

Download the file for your platform and install it as usual. On Linux you can also install the `.deb` with `sudo apt install ./<package>.deb`, or make the AppImage executable and run it directly.

> **No release published yet?** The Releases page is still empty — in that case, build from source (below) or check the page again later.

### Build from source — Windows

**Prerequisites**

- [Git](https://git-scm.com/download/win)
- [Rust](https://rustup.rs/) (the installer includes the required MSVC C++ build tools)
- [Node.js](https://nodejs.org/) 20 or newer
- `pnpm` — install once with `npm install -g pnpm` (the exact version is pinned by the project and picked up automatically)

**Build**

From a terminal, in the repository root:

```bat
git clone https://github.com/fuleshu/Aworkit.git
cd Aworkit

:: 1. Build the core (framework)
cargo build --workspace --release --locked

:: 2. Build the desktop app and its installers
cd desktop
pnpm install --frozen-lockfile
pnpm desktop:build
```

**Outputs**

| What | Where |
| --- | --- |
| Core binaries | `target\release\` |
| Desktop executable | `desktop\src-tauri\target\release\aworkit-desktop.exe` |
| Installers (`.msi`, setup `.exe`) | `desktop\src-tauri\target\release\bundle\` |

**Run it**

```bat
desktop\src-tauri\target\release\aworkit-desktop.exe
```

### Build from source — Linux

**Prerequisites**

- Rust (`rustup toolchain install stable` — the project needs Rust 1.97 or newer)
- Node.js 20 or newer, and `pnpm` (`npm install -g pnpm`)
- The system libraries required by the desktop shell (WebKitGTK 4.1 and friends):

```sh
sudo apt update
sudo apt install -y \
  libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```

> WebKitGTK **4.1** is required — the older 4.0 package will not work. Full Linux notes (AppImage/FUSE, ARM, verified versions): **[docs/linux-build.md](docs/linux-build.md)**.

**Build**

```sh
git clone https://github.com/fuleshu/Aworkit.git
cd Aworkit

# 1. Build the core (framework)
cargo build --workspace --release --locked

# 2. Build the desktop app and its installers
cd desktop
pnpm install --frozen-lockfile
pnpm desktop:build
```

**Outputs**

| What | Where |
| --- | --- |
| Core binaries | `target/release/` |
| Desktop executable | `desktop/src-tauri/target/release/aworkit-desktop` |
| Installers (`.deb`, `.rpm`, `.AppImage`) | `desktop/src-tauri/target/release/bundle/` |

**Run it**

```sh
./desktop/src-tauri/target/release/aworkit-desktop
```

The desktop app needs a graphical session and, on Linux, a running keyring (e.g. `gnome-keyring`) for secure credential storage.

## Development mode

For day-to-day work on the code, start the app in watch mode (rebuilds on change):

```sh
cd desktop
pnpm desktop:dev
```

Useful commands:

| Command | What it does |
| --- | --- |
| `cargo build --workspace --release --locked` | build the core only |
| `cd desktop && pnpm build` | type-check and build the UI only |
| `cd desktop && pnpm test` | run the UI test suite |
| `rebuild_win_nobundle.bat` | quick Windows rebuild without installers |

## Built with Adashi

This repository is developed with **[Adashi](https://github.com/fuleshu/Adashi)** — *the senior agent dashboard*. Adashi provides the project's design model, task list, QA jobs, and the lifecycle instructions that AI coding agents follow while working in this codebase (see [`agents.md`](agents.md)).

Aworkit and Adashi also fit together at runtime: Adashi can be added as an MCP server inside Aworkit, so your workflows can read and update Adashi's project memory, tasks, and design documents through its tools.

## Libraries and licenses

Aworkit itself is Apache-2.0. It stands on the shoulders of these major open-source libraries — thank you to their maintainers:

| Library | Used for | License |
| --- | --- | --- |
| [Tauri](https://github.com/tauri-apps/tauri) | desktop app shell (Rust + web UI) | Apache-2.0 OR MIT |
| [React](https://github.com/facebook/react) | user interface | MIT |
| [Mantine](https://github.com/mantinedev/mantine) | UI components | MIT |
| [React Flow](https://github.com/xyflow/xyflow) | workflow editor canvas | MIT |
| [TanStack Virtual](https://github.com/TanStack/virtual) | fast long lists | MIT |
| [react-markdown](https://github.com/remarkjs/react-markdown) / [remark-gfm](https://github.com/remarkjs/remark-gfm) | rendering chat messages | MIT |
| [Zod](https://github.com/colinhacks/zod) | data validation | MIT |
| [Vite](https://github.com/vitejs/vite) / [TypeScript](https://github.com/microsoft/TypeScript) / [Vitest](https://github.com/vitest-dev/vitest) | build, types, tests | MIT / Apache-2.0 / MIT |
| [tokio](https://github.com/tokio-rs/tokio) | async runtime (Rust) | MIT |
| [serde](https://github.com/serde-rs/serde) | (de)serialisation (Rust) | MIT OR Apache-2.0 |
| [reqwest](https://github.com/seanmonstar/reqwest) | HTTP client (Rust) | MIT OR Apache-2.0 |
| [rmcp](https://github.com/modelcontextprotocol/rust-sdk) | official MCP SDK (Rust) | Apache-2.0 (older parts still MIT) |
| [rusqlite](https://github.com/rusqlite/rusqlite) | local SQLite database | MIT |
| WebKitGTK / GTK | Linux UI backend | LGPL-2.1 / MIT |

The complete, version-pinned dependency lists live in [`Cargo.lock`](Cargo.lock) and [`desktop/pnpm-lock.yaml`](desktop/pnpm-lock.yaml). All listed licenses are permissive and compatible with Apache-2.0.

## License

Aworkit is released under the **[Apache License 2.0](LICENSE)**.

You are free to use, modify, and distribute it — commercially or not — provided that you retain the license and copyright notices.

## Contributing

Contributions are welcome — bug reports, feature ideas, and pull requests alike:

- 🐛 **Bug reports & ideas** → [open an issue](https://github.com/fuleshu/Aworkit/issues)
- 🔀 **Code changes** → fork the repo, create a branch, and open a pull request

Before a pull request, please make sure the existing checks still pass:

```sh
cargo build --workspace --release --locked
cd desktop && pnpm check && pnpm test
```

Project background and design notes are documented in [`aworkit_design_concept_final.md`](aworkit_design_concept_final.md), and platform-specific build details in [`docs/`](docs/).
