#!/usr/bin/env node
// Prune stale Cargo incremental-compilation cache.
//
// Cargo keeps one incremental cache directory per "unit flavour" - a crate plus the
// hash of the flags, features and target kind it was compiled with - and reclaims none
// of them. In this repository a flavour costs roughly 150-500 MB, dominated by
// dep-graph.bin, so a month of varied invocations (build, check, clippy, test,
// tauri dev, feature toggles) leaves tens of gigabytes of cache that will never be
// read again. Deepening the profile is not the answer: incremental compilation is what
// keeps edit/rebuild loops fast, so keep it and bound it instead.
//
// This script deletes incremental cache entries untouched for --days (default 14). It
// touches nothing else: deps/, build/ and .fingerprint/ are the artifact set Cargo
// genuinely reuses, so they are left to `cargo clean`.
//
// Usage:
//   node scripts/trim-rust-incremental.mjs                 # apply, 14 day retention
//   node scripts/trim-rust-incremental.mjs --dry-run       # report what would go
//   node scripts/trim-rust-incremental.mjs --days 3        # tighter retention
//   node scripts/trim-rust-incremental.mjs --all           # drop the whole cache
//
// A single invocation can also opt out without editing the manifests:
//   CARGO_INCREMENTAL=0 cargo build -p aworkit-capability-host
import { readdir, rm, stat } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

const args = process.argv.slice(2);
const dryRun = args.includes('--dry-run');
const wipeAll = args.includes('--all');
const daysIndex = args.indexOf('--days');
const days = daysIndex === -1 ? 14 : Number(args[daysIndex + 1]);
if (!Number.isFinite(days) || days < 0) {
  console.error('--days needs a non-negative number');
  process.exit(2);
}
const cutoff = Date.now() - days * 24 * 60 * 60 * 1000;

// Only these roots may ever be touched, whatever the arguments say.
const allowedRoots = [
  path.join(repoRoot, 'target'),
  path.join(repoRoot, 'desktop', 'src-tauri', 'target'),
];

const mb = (bytes) => `${(bytes / 1024 ** 2).toFixed(0)} MB`;

async function findIncrementalRoots() {
  const roots = [];
  for (const target of allowedRoots) {
    let profiles;
    try {
      profiles = await readdir(target, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const profile of profiles) {
      if (!profile.isDirectory()) continue;
      const dir = path.join(target, profile.name, 'incremental');
      try {
        if ((await stat(dir)).isDirectory()) roots.push(dir);
      } catch {
        // no incremental cache for this profile
      }
    }
  }
  return roots;
}

async function measure(dir) {
  let bytes = 0;
  let newest = 0;
  const walk = async (current) => {
    for (const entry of await readdir(current, { withFileTypes: true })) {
      const full = path.join(current, entry.name);
      if (entry.isDirectory()) await walk(full);
      else if (entry.isFile()) {
        const info = await stat(full);
        bytes += info.size;
        if (info.mtimeMs > newest) newest = info.mtimeMs;
      }
    }
  };
  await walk(dir);
  return { bytes, newest };
}

function assertInside(file, root) {
  const rel = path.relative(root, file);
  if (rel.startsWith('..') || path.isAbsolute(rel)) {
    throw new Error(`refusing to delete outside ${root}: ${file}`);
  }
}

let reclaimed = 0;
let kept = 0;
let removed = 0;
let keptCount = 0;

for (const incrementalRoot of await findIncrementalRoots()) {
  console.log(`\n${path.relative(repoRoot, incrementalRoot)}`);
  let printed = false;
  for (const flavour of await readdir(incrementalRoot, { withFileTypes: true })) {
    if (!flavour.isDirectory()) continue;
    const flavourDir = path.join(incrementalRoot, flavour.name);
    // Cargo stores sessions as s-<...> inside the flavour directory. Prune per session
    // so a recently used session survives next to an old sibling.
    const entries = await readdir(flavourDir, { withFileTypes: true });
    const sessions = entries.filter((e) => e.isDirectory() && e.name.startsWith('s-'));
    const candidates = sessions.length > 0 ? sessions : [flavour];

    for (const candidate of candidates) {
      const candidateDir = path.join(flavourDir, candidate.name);
      const { bytes, newest } = await measure(candidateDir);
      if (!wipeAll && newest >= cutoff) {
        kept += bytes;
        keptCount++;
        continue;
      }
      assertInside(candidateDir, incrementalRoot);
      if (!dryRun) await rm(candidateDir, { recursive: true, force: true });
      reclaimed += bytes;
      removed++;
      const when = newest ? new Date(newest).toISOString().slice(0, 10) : 'unknown';
      const what = sessions.length > 0 ? `${flavour.name}/${candidate.name}` : flavour.name;
      console.log(`  ${dryRun ? 'would remove' : 'removed'} ${what} (${mb(bytes)}, last used ${when})`);
      printed = true;
    }

    // A flavour directory only exists to hold its sessions.
    const remaining = await readdir(flavourDir);
    if (!dryRun && remaining.length === 0) await rm(flavourDir, { recursive: true, force: true });
  }
  if (!printed) console.log('  nothing stale');
}

console.log(
  `\n${dryRun ? 'would reclaim' : 'reclaimed'} ${mb(reclaimed)} from ${removed} cache entr${removed === 1 ? 'y' : 'ies'}` +
    (keptCount ? `, kept ${mb(kept)} across ${keptCount} entr${keptCount === 1 ? 'y' : 'ies'} used within ${days} day(s)` : ''),
);
if (dryRun) console.log('dry run: nothing was deleted');
