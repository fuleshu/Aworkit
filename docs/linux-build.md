# Building Aworkit on Linux

`readme.txt` gives the platform-neutral commands. This document adds the parts that are
Linux-specific and records what was actually run to verify them.

Verified on 2026-09-20, commit `a8f5c51`, on:

- Ubuntu 24.04.4 LTS (noble), `aarch64` (ARM64)
- rustc/cargo 1.97.1 (`stable-aarch64-unknown-linux-gnu`)
- node v22.23.1, npm 10.9.8
- pnpm 10.16.1 (the version pinned by `desktop/package.json` `packageManager`)
- Tauri CLI 2.11.4, Tauri crates 2.11.3/2.11.4

## 1. Prerequisites

### System libraries (Tauri v2 / WebKitGTK 4.1)

Ubuntu 22.04+, Debian 12+ and derivatives:

```sh
sudo apt update
sudo apt install -y \
  libwebkit2gtk-4.1-dev \
  build-essential \
  curl \
  wget \
  file \
  libxdo-dev \
  libssl-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev
```

WebKitGTK **4.1** is required by Tauri v2; `libwebkit2gtk-4.0-dev` will not link. These
were all already installed on the verified machine (`libwebkit2gtk-4.1-dev` 2.52.3,
`libgtk-3-dev` 3.24.41, `libxdo-dev`, `librsvg2-dev` 2.58, `libayatana-appindicator3-dev`
0.5.93, `libssl-dev` 3.0.13, `build-essential`, `pkg-config`, `file`, `wget`, `curl`).
`patchelf` is *not* needed.

To run the produced AppImage on Ubuntu 24.04 you also need FUSE 2, which the base system
no longer ships:

```sh
sudo apt install -y libfuse2t64        # or run the AppImage with --appimage-extract-and-run
```

### Rust

The workspace sets `rust-version = "1.97"` and `edition = "2024"`, so an older Rust cannot
build it. A rustup-managed stable toolchain is enough:

```sh
rustup toolchain install stable
```

`cargo` and `rustc` live in `~/.cargo/bin`. On the verified machine that directory was
**not** on `PATH` in a non-login shell, so every command below needs:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
```

Check with `cargo -V` (expected 1.97.x or newer).

### Node and pnpm

Node 20+ works; the verified machine has v22.23.1.

`desktop/package.json` pins `"packageManager": "pnpm@10.16.1"`. A stock pnpm honours that
field by downloading the pinned version and re-executing it (verified with pnpm 12.4.2,
which reported `Done in 3.5s using pnpm v10.16.1`), so installing pnpm once is enough — the
exact version does not have to be installed by hand:

```sh
export PATH="$HOME/.local/share/pnpm/bin:$PATH"   # where pnpm lives on the verified machine
pnpm --version                                    # prints 10.16.1 (the delegated pinned version)
```

### `desktop/pnpm-workspace.yaml` (fixed)

`onlyBuiltDependencies` is a configuration sequence. The repository used the scalar form:

```yaml
onlyBuiltDependencies: esbuild
```

pnpm 10.16.1 silently ignores that scalar; **pnpm 12 (verified with 12.4.2) fails to parse
the file**, so *every* pnpm command inside `desktop/` aborts before the pinned version can
be selected:

```text
Error: Failed to parse pnpm-workspace.yaml ... unexpected event: expected sequence start
```

It is now written as a sequence:

```yaml
onlyBuiltDependencies:
  - esbuild
```

With that, a stock pnpm 12 in `desktop/` delegates to the pinned 10.16.1 and
`pnpm install --frozen-lockfile` succeeds without rewriting `pnpm-lock.yaml`.

If `desktop/node_modules` was populated while the scalar form was in place, pnpm recorded
esbuild as an ignored build and keeps warning about it. Remove it once (esbuild still works,
because its platform binary arrives through the `@esbuild/linux-arm64` optional
dependency, but the warning is misleading):

```sh
rm -rf desktop/node_modules
```

## 2. Build

### Framework crates

From the repository root:

```sh
cargo build --workspace --release --locked
```

~3m23s on 20 cores. Outputs in `target/release/`:

| Binary | Purpose |
| --- | --- |
| `aworkit-trusted-core` | trusted application core |
| `aworkit-workflow-worker` | workflow runtime worker |
| `aworkit-capability-host` | capability and extension host |
| `aworkit-bootstrap-helper` | bootstrap/rollback helper |
| `aworkit-local-store` | local state maintenance tool |
| `aworkit-portable-store` | portable session tooling |
| `aworkit-release-assembler` | whole-application bundle assembler |

### Desktop application and installers

```sh
cd desktop
pnpm install --frozen-lockfile
pnpm desktop:build
```

`pnpm desktop:build` is `tauri build`. It first runs `beforeBuildCommand`
(`pnpm build` = `tsc --noEmit && vite build`, ~23s) and then compiles
`desktop/src-tauri` (~4m57s) and bundles every target configured by
`"targets": "all"`. On Linux that is deb, rpm and AppImage:

| Artifact | Size |
| --- | --- |
| `desktop/src-tauri/target/release/aworkit-desktop` | 76 MB |
| `desktop/src-tauri/target/release/bundle/deb/Aworkit_0.1.0_arm64.deb` | 42 MB |
| `desktop/src-tauri/target/release/bundle/rpm/Aworkit-0.1.0-1.aarch64.rpm` | 42 MB |
| `desktop/src-tauri/target/release/bundle/appimage/Aworkit_0.1.0_aarch64.AppImage` | 113 MB |

Frontend only (no Rust): `cd desktop && pnpm build`.

Development mode: `cd desktop && pnpm desktop:dev` (`tauri dev`).

## 3. Run

```sh
./desktop/src-tauri/target/release/aworkit-desktop
```

The desktop binary is self-contained: it links the trusted core, workflow worker and
capability host as libraries in-process, and `ldd` reports no unresolved shared objects.
The separate `target/release/aworkit-*` binaries belong to the packaged/activation model,
not to a plain desktop run.

- Needs a graphical session (`DISPLAY`/Wayland); it is a GTK/WebKitGTK application.
- Credential storage uses the Secret Service, so a running keyring (`gnome-keyring`,
  installed here) is expected.
- Install the deb with `sudo apt install ./bundle/deb/Aworkit_0.1.0_arm64.deb`, or make the
  AppImage executable and run it directly.

## 4. Reproducible whole-application bundle (optional)

`scripts/assemble-release-bundle.sh` produces the packed distribution layout instead of a
distro package:

```sh
scripts/assemble-release-bundle.sh                    # default release/aworkit-linux-aarch64
scripts/assemble-release-bundle.sh target/aworkit-linux-aarch64
```

It builds the four shipped binaries plus the release assembler with `--locked`, builds the
UI with `npm --prefix desktop run build` (npm, deliberately not pnpm, so it is unaffected by
pnpm workspace configuration), compiles `desktop/src-tauri` with `--locked`, and emits a
manifest carrying the source revision, source-tree hash, workspace-identity hash, toolchain
hash and `SOURCE_DATE_EPOCH` (`CARGO_INCREMENTAL=0`). Verified output layout:

```text
aworkit-linux-aarch64/
  bin/aworkit-desktop
  bin/aworkit-trusted-core
  bin/aworkit-workflow-worker
  bin/aworkit-capability-host
  bin/aworkit-bootstrap-helper
  ui/{index.html,assets/}
  WholeApplicationBundleV1.json     # platform, arch, per-entry sha256, bytes, executable bit
  BuildProvenanceV1.json            # source revision/tree hash, toolchain hash
```

The default output path is untracked and not covered by `.gitignore`; pass a path under
`target/` or add `release/` to `.gitignore`.

## 5. Notes and known issues

- **AppImage on ARM.** `linuxdeploy`, which Tauri uses, cannot cross-compile ARM AppImages,
  so an aarch64 AppImage can only be built on an ARM host (or emulator). This machine is
  ARM, so `targets: "all"` succeeds. See
  <https://v2.tauri.app/distribute/appimage/#appimages-for-arm-based-devices>.
- **Test binary is bundled.** `desktop/src-tauri/src/bin/aworkit-rescue-e2e.rs` is a test
  executable that Tauri packages as an app binary, so it lands in `usr/bin` of the deb/rpm
  (53 MB of the payload). Excluding it from bundling would shrink the installers.
- **Build warnings.** `crates/aworkit-capability-host/src/shell.rs` warns about an unused
  `mut` and an unused `program` parameter on Linux, and
  `desktop/src-tauri/src/main.rs` warns about an unused `mut` on `tauri::generate_context!()`.
  All are harmless.
- **Generated schema churn.** `tauri build` rewrites the tracked
  `desktop/src-tauri/gen/schemas/linux-schema.json`. Restore it if the diff is unwanted.
