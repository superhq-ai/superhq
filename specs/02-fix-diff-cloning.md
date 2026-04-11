# 02 — Stop cloning entire diff state on every watcher event

## Problem

Every file-change event triggers this (mod.rs:259-263):

```rust
let result = DiffResult {
    dirty_paths: dirty,
    files: cached_files.values().cloned().collect(),  // clones all ChangedFile
    diffs: cached_diffs.clone(),                      // clones ALL hunks + lines
};
```

For 8 files with ~170 lines of diff, this copies every hunk, every DiffLine
(with its content String), and every ChangedFile — on every single FS event.
During a `bun install` or `npm install` this fires hundreds of times.

## Fix

Send only the paths that changed, with their updated (or removed) entries.

### New DiffResult shape

```rust
struct DiffResult {
    /// Paths that were re-evaluated in this batch.
    dirty_paths: HashSet<String>,
    /// Updated or new entries. Missing key = file was removed.
    updated_files: HashMap<String, ChangedFile>,
    updated_diffs: HashMap<String, FileDiff>,
    /// Paths whose diff was removed (file reverted to original).
    removed_paths: HashSet<String>,
}
```

### Watcher side (bridge thread)

Instead of cloning the full cache each iteration:

```rust
let mut updated_files = HashMap::new();
let mut updated_diffs = HashMap::new();
let mut removed_paths = HashSet::new();

for path in &dirty {
    // ... read + compare ...
    match status {
        reverted => {
            cached_files.remove(path);
            cached_diffs.remove(path);
            removed_paths.insert(path.clone());
        }
        changed => {
            let diff = compute_file_diff(...);
            let file = ChangedFile { ... };
            cached_files.insert(path.clone(), file.clone());
            cached_diffs.insert(path.clone(), diff.clone());
            updated_files.insert(path.clone(), file);
            updated_diffs.insert(path.clone(), diff);
        }
    }
}
```

Only the dirty paths' data is cloned (once), not the full accumulated state.

### UI side (apply_diff_result)

`ChangesTab::apply_results` merges the delta instead of replacing wholesale:

```rust
fn apply_results(&mut self, result: DiffResult) {
    for path in &result.dirty_paths {
        self.suppressed.remove(path);
    }
    for path in &result.removed_paths {
        self.changed_files.retain(|f| f.path != *path);
        self.file_diffs.remove(path);
        self.highlight_cache.remove(path);
    }
    for (path, file) in result.updated_files {
        if self.suppressed.contains(&path) { continue; }
        // Update or insert into changed_files
        if let Some(existing) = self.changed_files.iter_mut().find(|f| f.path == path) {
            *existing = file;
        } else {
            self.changed_files.push(file);
        }
    }
    for (path, diff) in result.updated_diffs {
        if self.suppressed.contains(&path) { continue; }
        self.file_diffs.insert(path.clone(), diff);
        self.highlight_cache.remove(&path); // invalidate stale highlights
    }
}
```

## Impact

- During rapid FS events (npm install), only 1-2 files are dirty per batch
  instead of cloning all 8+ files' full diff data.
- `changed_files` Vec is mutated in place rather than rebuilt from scratch.
- Highlight cache is only invalidated for actually-changed paths.
