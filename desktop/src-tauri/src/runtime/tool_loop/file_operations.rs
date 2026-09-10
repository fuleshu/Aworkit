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
                let grep = files
                    .grep_v1(
                        &FileGrepRequestV1 {
                            pattern: pattern.to_owned(),
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
                });
                Ok((
                    value,
                    format!(
                        "Found {} match(es) across {} file(s).",
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
