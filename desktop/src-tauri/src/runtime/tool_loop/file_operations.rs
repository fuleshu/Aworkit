//! Bounded text-file operations against the directory authorized for one invocation.
use super::*;

impl FileToolDispatcherV1 {
    pub(super) fn execute_file(
        &self,
        files: &ProjectFiles,
        file_path: &Path,
        path: &str,
        cancellation: &CancellationToken,
    ) -> Result<(Value, String), String> {
        match &self.record.binding.limit {
            StoredFileToolLimitV1::Read { maximum_bytes } => {
                let offset = self.record.call.arguments.get("offset").and_then(Value::as_u64);
                let limit = self.record.call.arguments.get("limit").and_then(Value::as_u64);
                if offset.is_none() && limit.is_none() {
                    // A whole-file read keeps returning the file verbatim.
                    let read = files
                        .read_v1(
                            &FileReadRequestV1 {
                                path: file_path.to_owned(),
                                maximum_bytes: *maximum_bytes,
                            },
                            cancellation,
                        )
                        .map_err(|error| error.to_string())?;
                    let content = String::from_utf8(read.bytes)
                        .map_err(|_| "file is not UTF-8 text".to_owned())?;
                    let value = json!({
                        "path": path,
                        "content": content,
                        "contentHash": read.content_hash,
                        "bytes": read.effect.bytes_observed_or_written,
                    });
                    Ok((
                        value,
                        format!(
                            "Read {} bytes from {}.",
                            read.effect.bytes_observed_or_written,
                            read.effect.relative_path.display()
                        ),
                    ))
                } else {
                    // A paged read streams the selected line range and reports
                    // how to continue, so a large file is read in bounded steps.
                    let page = files
                        .read_lines_v1(
                            &FileLinesRequestV1 {
                                path: file_path.to_owned(),
                                offset_line: offset.unwrap_or(1),
                                maximum_lines: limit,
                                maximum_bytes: *maximum_bytes,
                            },
                            cancellation,
                        )
                        .map_err(|error| error.to_string())?;
                    let content = String::from_utf8(page.bytes)
                        .map_err(|_| "file is not UTF-8 text".to_owned())?;
                    let next_offset = page.first_line + page.lines;
                    let value = json!({
                        "path": path,
                        "content": content,
                        "contentHash": page.content_hash,
                        "bytes": page.effect.bytes_observed_or_written,
                        "firstLine": page.first_line,
                        "lines": page.lines,
                        "more": page.more,
                        "truncated": page.truncated,
                        "nextOffset": if page.more { json!(next_offset) } else { Value::Null },
                    });
                    let name = page.effect.relative_path.display();
                    let summary = if page.truncated {
                        format!(
                            "Read {} line(s) from {name} starting at line {}; the last line was cut at the content bound.",
                            page.lines, page.first_line
                        )
                    } else if page.more {
                        format!(
                            "Read {} line(s) from {name} starting at line {}; more lines follow, continue with offset {next_offset}.",
                            page.lines, page.first_line
                        )
                    } else {
                        format!(
                            "Read {} line(s) from {name} starting at line {}.",
                            page.lines, page.first_line
                        )
                    };
                    Ok((value, summary))
                }
            }
            StoredFileToolLimitV1::Search { maximum_results } => {
                let needle = self.record.call.arguments["query"]
                    .as_str()
                    .ok_or_else(|| "search query is invalid".to_owned())?;
                let search = files
                    .search_v1(
                        &FileSearchRequestV1 {
                            path: file_path.to_owned(),
                            needle: needle.to_owned(),
                            maximum_results: *maximum_results,
                        },
                        cancellation,
                    )
                    .map_err(|error| error.to_string())?;
                let match_count = search.offsets.len();
                let value = json!({
                    "path": path,
                    "query": needle,
                    "offsets": search.offsets,
                    "contentHash": search.effect.before_content_hash,
                    "bytesObserved": search.effect.bytes_observed_or_written,
                });
                Ok((
                    value,
                    format!(
                        "Found {} match(es) in {}.",
                        match_count,
                        search.effect.relative_path.display()
                    ),
                ))
            }
            StoredFileToolLimitV1::List { maximum_entries } => {
                let pattern = self.record.call.arguments["pattern"]
                    .as_str()
                    .ok_or_else(|| "glob pattern is invalid".to_owned())?;
                let list = files
                    .list_v1(
                        &FileListRequestV1 {
                            pattern: pattern.to_owned(),
                            maximum_entries: *maximum_entries,
                        },
                        cancellation,
                    )
                    .map_err(|error| error.to_string())?;
                let value = json!({
                    "pattern": pattern,
                    "entries": list.entries,
                });
                Ok((value, format!("Listed {} file(s).", list.entries.len())))
            }
            StoredFileToolLimitV1::Grep {
                maximum_matches,
                maximum_files,
            } => {
                let pattern = self.record.call.arguments["pattern"]
                    .as_str()
                    .ok_or_else(|| "regex pattern is invalid".to_owned())?;
                // The frozen file access already classified the target: its
                // relative path is the exact file or directory to search, and
                // "." means the whole resolved workspace.
                let scope = self
                    .record
                    .file_access
                    .as_ref()
                    .and_then(|access| access.as_ref().ok())
                    .map(|access| access.path.clone())
                    .filter(|path| path != Path::new("."));
                let grep = files
                    .grep_v1(
                        &FileGrepRequestV1 {
                            pattern: pattern.to_owned(),
                            path: scope,
                            maximum_matches: *maximum_matches,
                            maximum_files: *maximum_files,
                            maximum_file_bytes: 1024 * 1024,
                        },
                        cancellation,
                    )
                    .map_err(|error| error.to_string())?;
                let value = json!({
                    "pattern": pattern,
                    "matches": grep.matches,
                    "filesScanned": grep.files_scanned,
                    "matchLimitReached": grep.match_limit_reached,
                    "fileLimitReached": grep.file_limit_reached,
                    "timeLimitReached": grep.time_limit_reached,
                    "skippedDirectories": grep.skipped_directories,
                    "skippedFiles": grep.skipped_files,
                });
                // An incomplete scan must never read as "the pattern is absent".
                let completeness = if grep.file_limit_reached {
                    format!(
                        " The scan stopped at the {maximum_files} file limit, so deeper files were never examined; narrow the path or pattern and search again."
                    )
                } else if grep.time_limit_reached {
                    format!(
                        " The scan stopped at the {seconds}s time limit, so the tree was not exhausted; narrow the path or pattern and search again.",
                        seconds = aworkit_capability_host::MAX_GREP_WALL_CLOCK.as_secs()
                    )
                } else if grep.match_limit_reached {
                    format!(
                        " The scan stopped at the {maximum_matches} match limit, so more matches may exist; refine the pattern."
                    )
                } else {
                    String::new()
                };
                let skipped = if grep.skipped_directories == 0 && grep.skipped_files == 0 {
                    String::new()
                } else {
                    format!(
                        " Skipped {} generated, ignored or hidden directories and {} excluded, hidden or binary files.",
                        grep.skipped_directories, grep.skipped_files
                    )
                };
                Ok((
                    value,
                    format!(
                        "Found {} match(es) across {} file(s).{completeness}{skipped}",
                        grep.matches.len(),
                        grep.files_scanned
                    ),
                ))
            }
            StoredFileToolLimitV1::Edit { .. } => {
                let old_string = self.record.call.arguments["old_string"]
                    .as_str()
                    .ok_or_else(|| "old_string is invalid".to_owned())?;
                let new_string = self.record.call.arguments["new_string"]
                    .as_str()
                    .ok_or_else(|| "new_string is invalid".to_owned())?;
                let current = files
                    .read(file_path.to_owned())
                    .map_err(|error| error.to_string())?;
                let text = String::from_utf8(current.clone())
                    .map_err(|_| "file is not UTF-8 text".to_owned())?;
                let occurrences = text.match_indices(old_string).count();
                if occurrences == 0 {
                    return Err("old_string was not found in the file".to_owned());
                }
                if occurrences > 1 {
                    return Err("old_string matched more than once; make it unique".to_owned());
                }
                let replacement = text.replacen(old_string, new_string, 1).into_bytes();
                files
                    .edit_hash(
                        &file_path.to_owned(),
                        &content_hash_local(&current),
                        &replacement,
                    )
                    .map_err(|error| error.to_string())?;
                let value = json!({
                    "path": path,
                    "oldString": old_string,
                    "newString": new_string,
                    "contentHash": content_hash_local(&replacement),
                    "bytesWritten": replacement.len(),
                });
                Ok((
                    value,
                    format!("Edited {} by replacing one occurrence.", path),
                ))
            }
            StoredFileToolLimitV1::Write { .. } => {
                let content = self.record.call.arguments["content"]
                    .as_str()
                    .ok_or_else(|| "content is invalid".to_owned())?;
                let write = files
                    .write_v1(
                        &FileWriteRequestV1 {
                            path: file_path.to_owned(),
                            content: content.as_bytes().to_vec(),
                            expected_content_hash: None,
                        },
                        cancellation,
                    )
                    .map_err(|error| error.to_string())?;
                let value = json!({
                    "path": path,
                    "contentHash": write.effect.after_content_hash,
                    "bytesWritten": write.effect.bytes_observed_or_written,
                });
                Ok((value, format!("Wrote {} bytes to {}.", content.len(), path)))
            }
            _ => Err("not a text-file operation".into()),
        }
    }
}
