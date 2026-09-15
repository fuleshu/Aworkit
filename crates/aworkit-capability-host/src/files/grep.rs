//! Bounded, ignore-aware regex search across the project tree.
//!
//! The walk asks three questions before it spends work on an entry: do the
//! project's own ignore files exclude it, is it generated or hidden, and is it
//! text at all. What survives is read in parallel, and the whole search is
//! bounded by matches, files and wall-clock time. The result reports which bound
//! stopped it, because an incomplete scan must never read as "the pattern is
//! absent from this tree".

mod ignore_rules;

use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use regex::Regex;
use serde::{Deserialize, Serialize};

use self::ignore_rules::{IgnoreRules, is_generated_directory};
use super::{
    FileEffectDescriptorV1, FileEffectKindV1, FileToolError, MAX_FILE_BYTES, ProjectFiles,
    check_cancelled, content_hash, truncate_line,
};
use crate::CancellationToken;

/// Most matches one walk returns before it stops.
const MAX_GREP_MATCHES: usize = 512;

/// Most files one walk examines before it stops. The budget bounds work; it is
/// not a search strategy. A real repository holds far more files than this, and a
/// small cap silently turned "no matches" into a wrong answer, so the ceiling is
/// high enough to walk a whole project's source.
const MAX_GREP_FILES: usize = 20_000;

/// Longest wall-clock time one walk may spend.
///
/// The file and match ceilings bound what a walk returns, not how long it takes:
/// a tree of hundreds of thousands of generated files can stay inside both while
/// keeping a walk busy for minutes. The agent loop waits for the whole result, so
/// an unbounded duration reaches the user as a frozen tool. This ceiling turns the
/// worst case into a bounded pause that the result reports honestly. It is checked
/// before every candidate, inside every bounded file read, and between scan
/// blocks, so the search stops within one I/O step of the deadline.
pub const MAX_GREP_WALL_CLOCK: Duration = Duration::from_secs(30);

/// Bytes sniffed for a NUL byte before a candidate is treated as binary. A file
/// that is not text cannot match a line pattern, and scanning it costs a full
/// read plus a failed UTF-8 decode.
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

/// Largest ignore declaration read through the capability. Declarations are
/// small configuration; an oversized one is treated as absent.
const MAXIMUM_CONFIGURATION_BYTES: usize = 256 * 1024;

/// Candidate files one worker claims at a time. Blocks keep the parallel scan
/// load-balanced while leaving the result a deterministic prefix of the tree
/// order, whatever order the workers happen to finish in.
const SCAN_BLOCK_FILES: usize = 64;

/// Most worker threads one walk uses for reading candidate contents.
const MAXIMUM_SCAN_WORKERS: usize = 8;

/// Below this many candidates, threads cost more than they save.
const PARALLEL_SCAN_MINIMUM_FILES: usize = 128;

/// Bounded regex search across files beneath the root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileGrepRequestV1 {
    pub pattern: String,
    /// Root-relative scope: one file to search, or a directory to walk. `None`
    /// searches the whole root.
    pub path: Option<PathBuf>,
    pub maximum_matches: usize,
    pub maximum_files: usize,
    pub maximum_file_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileGrepResultV1 {
    pub matches: Vec<FileGrepMatchV1>,
    pub files_scanned: usize,
    /// True when the scan stopped at the match ceiling, so the matches are not
    /// the complete set for the pattern.
    pub match_limit_reached: bool,
    /// True when the scan stopped at the file ceiling, so whole subtrees were
    /// never examined and "no matches" does not mean the pattern is absent.
    pub file_limit_reached: bool,
    /// True when the scan stopped at the wall-clock ceiling, so the tree was not
    /// exhausted and "no matches" does not mean the pattern is absent.
    pub time_limit_reached: bool,
    /// Generated, ignored or hidden directories that were deliberately skipped.
    pub skipped_directories: usize,
    /// Hidden, ignored, oversized or binary files that were deliberately skipped.
    pub skipped_files: usize,
    pub effect: FileEffectDescriptorV1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileGrepMatchV1 {
    pub path: String,
    pub line: u64,
    pub offset: usize,
    pub line_text: String,
}

/// What one candidate file contributed to the scan.
enum CandidateRead {
    /// Bounded UTF-8 text to search.
    Text(String),
    /// Binary, oversized, unreadable, or not text: counted, never searched.
    Skipped,
    /// The wall-clock budget expired while reading it.
    Expired,
}

/// The candidate files one walk will search, in the order the tree was visited.
#[derive(Default)]
struct SearchPlan {
    candidates: Vec<PathBuf>,
    skipped_directories: usize,
    skipped_files: usize,
    file_limit_reached: bool,
    time_limit_reached: bool,
}

/// What the read phase found across all candidates.
#[derive(Default)]
struct ScanOutcome {
    matches: Vec<FileGrepMatchV1>,
    files_scanned: usize,
    skipped_files: usize,
    match_limit_reached: bool,
    time_limit_reached: bool,
}

/// One block's local result, merged into the ordered prefix only when complete.
#[derive(Default)]
struct BlockOutcome {
    matches: Vec<FileGrepMatchV1>,
    files_scanned: usize,
    skipped_files: usize,
}

impl ProjectFiles {
    /// Regex search across text files beneath the root with line context, bounded
    /// by the default wall-clock ceiling and scanned with every available core.
    pub fn grep_v1(
        &self,
        request: &FileGrepRequestV1,
        cancellation: &CancellationToken,
    ) -> Result<FileGrepResultV1, FileToolError> {
        self.grep_v1_with_budget(request, cancellation, MAX_GREP_WALL_CLOCK)
    }

    /// Regex search with an explicit wall-clock budget.
    ///
    /// `Duration::ZERO` expires before the first file is read, which is how the
    /// time bound is covered deterministically by tests.
    pub fn grep_v1_with_budget(
        &self,
        request: &FileGrepRequestV1,
        cancellation: &CancellationToken,
        budget: Duration,
    ) -> Result<FileGrepResultV1, FileToolError> {
        self.grep_v1_with_budget_and_workers(request, cancellation, budget, default_scan_workers())
    }

    /// Regex search with an explicit budget and worker count. Tests use it to
    /// compare a single-worker walk against the parallel one, which must return
    /// the same matches in the same order.
    pub fn grep_v1_with_budget_and_workers(
        &self,
        request: &FileGrepRequestV1,
        cancellation: &CancellationToken,
        budget: Duration,
        workers: usize,
    ) -> Result<FileGrepResultV1, FileToolError> {
        validate_grep_request(request)?;
        let pattern = Regex::new(&request.pattern).map_err(|_| FileToolError::InvalidGrep)?;
        self.revalidate_root()?;
        // An unrepresentable deadline (a budget far beyond the clock's range) is
        // treated as no deadline rather than as an immediately expired one.
        let deadline = Instant::now().checked_add(budget);
        let plan = self.plan_search(request, deadline, cancellation)?;
        let scan = self.scan_candidates(&plan, &pattern, request, deadline, cancellation, workers)?;
        Ok(FileGrepResultV1 {
            effect: FileEffectDescriptorV1 {
                kind: FileEffectKindV1::Grep,
                relative_path: PathBuf::new(),
                before_content_hash: content_hash(&[]),
                after_content_hash: content_hash(&[]),
                bytes_observed_or_written: scan.files_scanned,
                write_committed: false,
            },
            match_limit_reached: scan.match_limit_reached,
            file_limit_reached: plan.file_limit_reached,
            time_limit_reached: plan.time_limit_reached || scan.time_limit_reached,
            skipped_directories: plan.skipped_directories,
            skipped_files: plan.skipped_files.saturating_add(scan.skipped_files),
            matches: scan.matches,
            files_scanned: scan.files_scanned,
        })
    }

    /// Resolves the request into the exact candidate files to read.
    fn plan_search(
        &self,
        request: &FileGrepRequestV1,
        deadline: Option<Instant>,
        cancellation: &CancellationToken,
    ) -> Result<SearchPlan, FileToolError> {
        let mut plan = SearchPlan::default();
        let scope = request
            .path
            .as_deref()
            .filter(|path| !path.as_os_str().is_empty() && *path != Path::new("."));
        let Some(scope) = scope else {
            let mut rules = IgnoreRules::for_search(self, &self.authority.root, Path::new(""));
            self.walk(
                &Path::new(""),
                &mut rules,
                request,
                &mut plan,
                deadline,
                cancellation,
            )?;
            return Ok(plan);
        };
        let scope = super::validate_relative(scope)?;
        // The metadata probe precedes the symlink walk so a scope that simply
        // does not exist is named as such instead of surfacing a bare I/O error.
        let metadata = match self.directory.symlink_metadata(scope) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(FileToolError::MissingPath(scope.display().to_string()));
            }
            Err(error) => return Err(FileToolError::Io(error)),
        };
        self.reject_symlinks(scope, true)?;
        if metadata.is_dir() {
            let mut rules = IgnoreRules::for_search(self, &self.authority.root, scope);
            self.walk(scope, &mut rules, request, &mut plan, deadline, cancellation)?;
        } else if metadata.is_file() {
            // An explicitly named file is searched whatever its name or size
            // convention says: the caller already chose it.
            plan.candidates.push(scope.to_path_buf());
        }
        Ok(plan)
    }

    /// Depth-first walk that collects searchable candidates. Entries the project
    /// excludes, generated trees and hidden names are counted, never read.
    fn walk(
        &self,
        directory_path: &Path,
        rules: &mut IgnoreRules,
        request: &FileGrepRequestV1,
        plan: &mut SearchPlan,
        deadline: Option<Instant>,
        cancellation: &CancellationToken,
    ) -> Result<(), FileToolError> {
        if plan.file_limit_reached || plan.time_limit_reached {
            return Ok(());
        }
        if expired(deadline) {
            plan.time_limit_reached = true;
            return Ok(());
        }
        check_cancelled(cancellation)?;
        let directory = if directory_path.as_os_str().is_empty() {
            self.directory.clone()
        } else {
            self.reject_symlinks(directory_path, false)?;
            match self.directory.open_dir(directory_path) {
                Ok(directory) => Arc::new(directory),
                Err(_) => return Ok(()),
            }
        };
        for entry in directory.entries().map_err(FileToolError::Io)? {
            if plan.file_limit_reached || plan.time_limit_reached {
                return Ok(());
            }
            if expired(deadline) {
                plan.time_limit_reached = true;
                return Ok(());
            }
            let entry = entry.map_err(FileToolError::Io)?;
            let name = entry.file_name().to_string_lossy().to_string();
            let relative = directory_path.join(&name);
            let file_type = entry.file_type().map_err(FileToolError::Io)?;
            if file_type.is_dir() {
                if name.starts_with('.')
                    || is_generated_directory(&name)
                    || rules.is_ignored(&self.authority.root, &relative, true)
                {
                    plan.skipped_directories = plan.skipped_directories.saturating_add(1);
                    continue;
                }
                let entered = rules.enter(self, &self.authority.root, &relative);
                let result = self.walk(&relative, rules, request, plan, deadline, cancellation);
                rules.leave(entered);
                result?;
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            if name.starts_with('.') || rules.is_ignored(&self.authority.root, &relative, false) {
                plan.skipped_files = plan.skipped_files.saturating_add(1);
                continue;
            }
            if plan.candidates.len() >= request.maximum_files {
                plan.file_limit_reached = true;
                return Ok(());
            }
            plan.candidates.push(relative);
        }
        Ok(())
    }

    /// Reads the planned candidates, in parallel when the tree is large enough to
    /// pay for the threads, and returns the matches as the deterministic prefix of
    /// the tree order that the bounds allow.
    fn scan_candidates(
        &self,
        plan: &SearchPlan,
        pattern: &Regex,
        request: &FileGrepRequestV1,
        deadline: Option<Instant>,
        cancellation: &CancellationToken,
        workers: usize,
    ) -> Result<ScanOutcome, FileToolError> {
        if plan.candidates.is_empty() {
            return Ok(ScanOutcome::default());
        }
        if workers <= 1 || plan.candidates.len() < PARALLEL_SCAN_MINIMUM_FILES {
            return self.scan_serially(&plan.candidates, pattern, request, deadline, cancellation);
        }
        self.scan_in_parallel(
            &plan.candidates,
            pattern,
            request,
            deadline,
            cancellation,
            workers.min(MAXIMUM_SCAN_WORKERS),
        )
    }

    /// One thread reads every candidate in order. This is the reference the
    /// parallel scan has to reproduce.
    fn scan_serially(
        &self,
        candidates: &[PathBuf],
        pattern: &Regex,
        request: &FileGrepRequestV1,
        deadline: Option<Instant>,
        cancellation: &CancellationToken,
    ) -> Result<ScanOutcome, FileToolError> {
        let mut outcome = ScanOutcome::default();
        for candidate in candidates {
            if expired(deadline) {
                outcome.time_limit_reached = true;
                break;
            }
            check_cancelled(cancellation)?;
            match self.read_candidate(candidate, request.maximum_file_bytes, deadline, cancellation)?
            {
                CandidateRead::Text(text) => {
                    outcome.files_scanned = outcome.files_scanned.saturating_add(1);
                    collect_matches(
                        &text,
                        candidate,
                        pattern,
                        &mut outcome.matches,
                        request.maximum_matches,
                    );
                }
                CandidateRead::Skipped => {
                    outcome.files_scanned = outcome.files_scanned.saturating_add(1);
                    outcome.skipped_files = outcome.skipped_files.saturating_add(1);
                }
                CandidateRead::Expired => {
                    outcome.time_limit_reached = true;
                    break;
                }
            }
            if outcome.matches.len() >= request.maximum_matches {
                outcome.match_limit_reached = true;
                break;
            }
        }
        Ok(outcome)
    }

    /// Workers claim blocks of candidates. A block only joins the result once it
    /// is complete, so the matches are always the prefix of the tree order the
    /// bounds allowed, never a race-dependent subset of it.
    fn scan_in_parallel(
        &self,
        candidates: &[PathBuf],
        pattern: &Regex,
        request: &FileGrepRequestV1,
        deadline: Option<Instant>,
        cancellation: &CancellationToken,
        workers: usize,
    ) -> Result<ScanOutcome, FileToolError> {
        let blocks = candidates.len().div_ceil(SCAN_BLOCK_FILES);
        let next_block = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        let expired_scan = AtomicBool::new(false);
        let cancelled_scan = AtomicBool::new(false);
        let completed: Mutex<BTreeMap<usize, BlockOutcome>> = Mutex::new(BTreeMap::new());
        let failure: Mutex<Option<FileToolError>> = Mutex::new(None);
        std::thread::scope(|scope| {
            for _ in 0..workers {
                let files = self.clone();
                let next_block = &next_block;
                let stop = &stop;
                let expired_scan = &expired_scan;
                let cancelled_scan = &cancelled_scan;
                let completed = &completed;
                let failure = &failure;
                scope.spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        if cancellation.is_cancelled() {
                            cancelled_scan.store(true, Ordering::Relaxed);
                            stop.store(true, Ordering::Relaxed);
                            break;
                        }
                        if expired(deadline) {
                            expired_scan.store(true, Ordering::Relaxed);
                            stop.store(true, Ordering::Relaxed);
                            break;
                        }
                        let block = next_block.fetch_add(1, Ordering::Relaxed);
                        if block >= blocks {
                            break;
                        }
                        let start = block * SCAN_BLOCK_FILES;
                        let end = (start + SCAN_BLOCK_FILES).min(candidates.len());
                        let mut outcome = BlockOutcome::default();
                        let mut complete = true;
                        for candidate in &candidates[start..end] {
                            // A block another worker stopped is discarded whole:
                            // a partial block would break the ordered prefix.
                            if stop.load(Ordering::Relaxed) {
                                complete = false;
                                break;
                            }
                            if expired(deadline) {
                                expired_scan.store(true, Ordering::Relaxed);
                                complete = false;
                                break;
                            }
                            match files.read_candidate(
                                candidate,
                                request.maximum_file_bytes,
                                deadline,
                                cancellation,
                            ) {
                                Ok(CandidateRead::Text(text)) => {
                                    outcome.files_scanned =
                                        outcome.files_scanned.saturating_add(1);
                                    collect_matches(
                                        &text,
                                        candidate,
                                        pattern,
                                        &mut outcome.matches,
                                        request.maximum_matches,
                                    );
                                }
                                Ok(CandidateRead::Skipped) => {
                                    outcome.files_scanned =
                                        outcome.files_scanned.saturating_add(1);
                                    outcome.skipped_files =
                                        outcome.skipped_files.saturating_add(1);
                                }
                                Ok(CandidateRead::Expired) => {
                                    expired_scan.store(true, Ordering::Relaxed);
                                    complete = false;
                                    break;
                                }
                                Err(FileToolError::Cancelled) => {
                                    cancelled_scan.store(true, Ordering::Relaxed);
                                    complete = false;
                                    break;
                                }
                                Err(error) => {
                                    if let Ok(mut slot) = failure.lock() {
                                        slot.get_or_insert(error);
                                    }
                                    complete = false;
                                    break;
                                }
                            }
                        }
                        if !complete {
                            stop.store(true, Ordering::Relaxed);
                            break;
                        }
                        let prefix_matches = {
                            let mut map = match completed.lock() {
                                Ok(map) => map,
                                Err(poisoned) => poisoned.into_inner(),
                            };
                            map.insert(block, outcome);
                            let mut prefix = 0_usize;
                            let mut index = 0_usize;
                            while let Some(block) = map.get(&index) {
                                prefix = prefix.saturating_add(block.matches.len());
                                index += 1;
                            }
                            prefix
                        };
                        // Stop once the ordered prefix alone carries the ceiling:
                        // later blocks could only be dropped again.
                        if prefix_matches >= request.maximum_matches {
                            stop.store(true, Ordering::Relaxed);
                        }
                    }
                });
            }
        });
        if let Ok(mut slot) = failure.lock() {
            if let Some(error) = slot.take() {
                return Err(error);
            }
        }
        if cancelled_scan.load(Ordering::Relaxed) {
            return Err(FileToolError::Cancelled);
        }
        let mut outcome = ScanOutcome {
            time_limit_reached: expired_scan.load(Ordering::Relaxed),
            ..ScanOutcome::default()
        };
        let mut map = match completed.into_inner() {
            Ok(map) => map,
            Err(poisoned) => poisoned.into_inner(),
        };
        for index in 0..blocks {
            let Some(block) = map.remove(&index) else {
                // The prefix ends at the first block no worker completed.
                break;
            };
            outcome.files_scanned = outcome.files_scanned.saturating_add(block.files_scanned);
            outcome.skipped_files = outcome.skipped_files.saturating_add(block.skipped_files);
            for found in block.matches {
                if outcome.matches.len() >= request.maximum_matches {
                    outcome.match_limit_reached = true;
                    break;
                }
                outcome.matches.push(found);
            }
            if outcome.matches.len() >= request.maximum_matches {
                outcome.match_limit_reached = true;
                break;
            }
        }
        Ok(outcome)
    }

    /// Reads one candidate as bounded UTF-8 text, or says why it was skipped.
    ///
    /// This is deliberately not `read_v1`: the root was already revalidated once
    /// for the whole search, the walk only descends through directory entries it
    /// checked for symlinks, and a regex walk has no use for the content hashes a
    /// requested read has to report. Paying that per-file authority and hashing
    /// cost is what made a wide walk take minutes.
    fn read_candidate(
        &self,
        relative: &Path,
        maximum_bytes: usize,
        deadline: Option<Instant>,
        cancellation: &CancellationToken,
    ) -> Result<CandidateRead, FileToolError> {
        check_cancelled(cancellation)?;
        // The final component is re-checked here: a file swapped for a symlink
        // between enumeration and open must not be followed out of the root.
        let metadata = match self.directory.symlink_metadata(relative) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(CandidateRead::Skipped);
            }
            Err(error) => return Err(FileToolError::Io(error)),
        };
        if metadata.file_type().is_symlink() {
            return Ok(CandidateRead::Skipped);
        }
        let Ok(mut file) = self.directory.open(relative) else {
            return Ok(CandidateRead::Skipped);
        };
        let Ok(length) = file.metadata().map(|metadata| metadata.len()) else {
            return Ok(CandidateRead::Skipped);
        };
        if length > maximum_bytes as u64 {
            return Ok(CandidateRead::Skipped);
        }
        let mut body = Vec::with_capacity(length as usize);
        let mut chunk = [0_u8; 8192];
        let mut binary = false;
        loop {
            // The deadline is honored inside one file too, so a single large or
            // slow read cannot run past the budget the result reports.
            if expired(deadline) {
                return Ok(CandidateRead::Expired);
            }
            check_cancelled(cancellation)?;
            let Ok(count) = file.read(&mut chunk) else {
                return Ok(CandidateRead::Skipped);
            };
            if count == 0 {
                break;
            }
            if body.len() < BINARY_SNIFF_BYTES && chunk[..count].contains(&0) {
                binary = true;
                break;
            }
            if body.len().saturating_add(count) > maximum_bytes {
                return Ok(CandidateRead::Skipped);
            }
            body.extend_from_slice(&chunk[..count]);
        }
        if binary {
            return Ok(CandidateRead::Skipped);
        }
        match String::from_utf8(body) {
            Ok(text) => Ok(CandidateRead::Text(text)),
            Err(_) => Ok(CandidateRead::Skipped),
        }
    }

    /// Reads one small configuration file through the capability. Ignore
    /// declarations are configuration, never search results, so every failure —
    /// absent, unreadable, oversized, not text, or a symlink — is simply absence.
    pub(super) fn read_configuration(&self, relative: &Path) -> Option<String> {
        let metadata = self.directory.symlink_metadata(relative).ok()?;
        if !metadata.is_file() {
            return None;
        }
        let file = self.directory.open(relative).ok()?;
        let mut body = Vec::new();
        file.take(MAXIMUM_CONFIGURATION_BYTES as u64)
            .read_to_end(&mut body)
            .ok()?;
        String::from_utf8(body).ok()
    }
}

/// Records every match of one already-counted candidate file.
fn collect_matches(
    text: &str,
    relative: &Path,
    pattern: &Regex,
    matches: &mut Vec<FileGrepMatchV1>,
    maximum_matches: usize,
) {
    let path_text = relative.to_string_lossy().replace('\\', "/");
    for (line_index, line) in text.lines().enumerate() {
        for found in pattern.find_iter(line) {
            if matches.len() >= maximum_matches {
                return;
            }
            matches.push(FileGrepMatchV1 {
                path: path_text.clone(),
                line: line_index as u64 + 1,
                offset: found.start(),
                line_text: truncate_line(line, 256),
            });
        }
    }
}

/// True when the wall-clock ceiling has passed.
fn expired(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

/// Worker count for one scan: every available core, bounded so one tool call
/// cannot occupy the whole machine.
fn default_scan_workers() -> usize {
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
        .min(MAXIMUM_SCAN_WORKERS)
}

/// Rejects a request whose pattern or bounds cannot be honored.
fn validate_grep_request(request: &FileGrepRequestV1) -> Result<(), FileToolError> {
    if request.pattern.is_empty()
        || request.pattern.len() > 16 * 1024
        || request.maximum_matches == 0
        || request.maximum_matches > MAX_GREP_MATCHES
        || request.maximum_files == 0
        || request.maximum_files > MAX_GREP_FILES
        || request.maximum_file_bytes == 0
        || request.maximum_file_bytes > MAX_FILE_BYTES
    {
        return Err(FileToolError::InvalidGrep);
    }
    Ok(())
}
