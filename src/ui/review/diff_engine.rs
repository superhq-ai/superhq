use shuru_sdk::AsyncSandbox;
use similar::{ChangeTag, TextDiff};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq)]
pub enum DiffLineKind {
    Context,
    Addition,
    Deletion,
}

#[derive(Clone)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_lineno: Option<usize>,
    pub new_lineno: Option<usize>,
    pub content: String,
}

#[derive(Clone)]
pub struct DiffHunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone)]
pub struct FileDiff {
    pub additions: usize,
    pub deletions: usize,
    pub hunks: Vec<DiffHunk>,
    pub is_binary: bool,
}

impl FileDiff {
    pub fn is_empty(&self) -> bool {
        !self.is_binary && self.additions == 0 && self.deletions == 0
    }
}

#[derive(Clone, Default)]
pub struct DiffStats {
    pub additions: usize,
    pub deletions: usize,
}

pub async fn read_host_file(
    rel_path: &str,
    host_mount_path: &str,
) -> Option<Vec<u8>> {
    let full_path = format!("{}/{}", host_mount_path, rel_path);
    tokio::fs::read(&full_path).await.ok()
}

pub async fn read_sandbox_file(
    rel_path: &str,
    workspace: &str,
    sandbox: &Arc<AsyncSandbox>,
) -> Option<Vec<u8>> {
    let full_path = format!("{}/{}", workspace, rel_path);
    sandbox.read_file(&full_path).await.ok()
}

pub async fn copy_to_host(
    rel_path: &str,
    host_mount_path: &str,
    sandbox: &Arc<AsyncSandbox>,
) -> Result<(), String> {
    // Safety: reject traversal attacks
    if rel_path.contains("..") {
        return Err("path contains '..'".into());
    }
    // Safety: don't overwrite git internals
    if rel_path.starts_with(".git/") || rel_path == ".git" {
        return Err("refusing to write to .git".into());
    }

    let sandbox_path = format!("/workspace/{}", rel_path);
    let host_path = format!("{}/{}", host_mount_path, rel_path);

    let content = sandbox.read_file(&sandbox_path).await
        .map_err(|e| format!("read sandbox file: {e}"))?;

    if let Some(parent) = std::path::Path::new(&host_path).parent() {
        tokio::fs::create_dir_all(parent).await
            .map_err(|e| format!("create dirs: {e}"))?;
    }

    tokio::fs::write(&host_path, &content).await
        .map_err(|e| format!("write host file: {e}"))?;

    Ok(())
}

fn is_binary(data: &[u8]) -> bool {
    let check_len = data.len().min(8192);
    data[..check_len].contains(&0)
}

/// Stats-only variant of `compute_file_diff`. Runs the same line diff but
/// doesn't materialize `DiffHunk`/`DiffLine` vecs — use when the caller only
/// needs addition/deletion counts (e.g. header totals for unexpanded rows).
pub fn compute_file_stats(old: &[u8], new: &[u8]) -> (DiffStats, bool) {
    if is_binary(old) || is_binary(new) {
        return (DiffStats::default(), true);
    }

    let old_text = String::from_utf8_lossy(old);
    let new_text = String::from_utf8_lossy(new);
    let diff = TextDiff::from_lines(old_text.as_ref(), new_text.as_ref());

    let mut additions = 0usize;
    let mut deletions = 0usize;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => additions += 1,
            ChangeTag::Delete => deletions += 1,
            ChangeTag::Equal => {}
        }
    }
    (DiffStats { additions, deletions }, false)
}

pub fn compute_file_diff(old: &[u8], new: &[u8]) -> FileDiff {
    if is_binary(old) || is_binary(new) {
        return FileDiff {
            additions: 0,
            deletions: 0,
            hunks: Vec::new(),
            is_binary: true,
        };
    }

    let old_text = String::from_utf8_lossy(old);
    let new_text = String::from_utf8_lossy(new);

    let diff = TextDiff::from_lines(old_text.as_ref(), new_text.as_ref());
    let mut hunks = Vec::new();
    let mut total_add = 0usize;
    let mut total_del = 0usize;

    for group in diff.grouped_ops(1) {
        let mut hunk_lines = Vec::new();
        let mut old_start = 0;
        let mut new_start = 0;
        let mut old_count = 0;
        let mut new_count = 0;
        let mut first = true;

        for op in &group {
            for change in diff.iter_changes(op) {
                let old_ln = change.old_index().map(|i| i + 1);
                let new_ln = change.new_index().map(|i| i + 1);

                if first {
                    old_start = old_ln.unwrap_or(1);
                    new_start = new_ln.unwrap_or(1);
                    first = false;
                }

                let kind = match change.tag() {
                    ChangeTag::Equal => {
                        old_count += 1;
                        new_count += 1;
                        DiffLineKind::Context
                    }
                    ChangeTag::Insert => {
                        new_count += 1;
                        total_add += 1;
                        DiffLineKind::Addition
                    }
                    ChangeTag::Delete => {
                        old_count += 1;
                        total_del += 1;
                        DiffLineKind::Deletion
                    }
                };

                hunk_lines.push(DiffLine {
                    kind,
                    old_lineno: old_ln,
                    new_lineno: new_ln,
                    content: change.to_string_lossy().to_string(),
                });
            }
        }

        hunks.push(DiffHunk {
            old_start,
            old_count,
            new_start,
            new_count,
            lines: hunk_lines,
        });
    }

    FileDiff {
        additions: total_add,
        deletions: total_del,
        hunks,
        is_binary: false,
    }
}

