// Shared staging for the native QA harness.
//
// Every smoke script needs a launchable `aworkit-desktop.exe` inside its own run
// directory, because the binary under src-tauri/target/debug can be locked by a
// running instance. Copying it cost ~85 MB per run and left tens of gigabytes of
// duplicate images behind in src-tauri/target. A hardlink gives each run its own
// path to the same image without duplicating the bytes: Cargo never rewrites a
// binary in place, it replaces the directory entry, so the run keeps the build it
// started with. The copy stays as a fallback for sources on another volume or on a
// filesystem without hardlinks.
import { copyFile, link } from 'node:fs/promises';

export async function stageBinary(source, destination) {
  try {
    await link(source, destination);
  } catch {
    await copyFile(source, destination);
  }
}
