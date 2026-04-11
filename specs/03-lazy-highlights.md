# 03 — Lazy highlight computation

## Problem

`changes_tab.rs:211-217` eagerly computes tree-sitter highlights for ALL
files on every render, even collapsed ones:

```rust
for file in &self.changed_files {
    if !self.highlight_cache.contains_key(&file.path) {
        if let Some(diff) = self.file_diffs.get(&file.path) {
            let runs = diff_view::compute_highlights(&file.path, &diff.hunks);
            self.highlight_cache.insert(file.path.clone(), runs);
        }
    }
}
```

`compute_highlights` (diff_view.rs:113-174) runs tree-sitter parsing: builds
a rope, creates a SyntaxHighlighter, parses the full concatenated diff text,
then extracts per-line spans. This is expensive — and completely wasted for
collapsed files that the user may never expand.

With 8 files, this runs 8 tree-sitter parses on the first render frame.

## Fix

Only compute highlights when a file is expanded for the first time.

Remove the eager loop (lines 211-217). Move highlight computation into
the per-file render block, gated on expansion:

```rust
for file in &self.changed_files {
    let expanded = self.expanded.entry(file.path.clone())
        .or_insert_with(|| Rc::new(Cell::new(false)));

    // Only compute highlights when expanded AND not cached
    if expanded.get() && !self.highlight_cache.contains_key(&file.path) {
        if let Some(diff) = self.file_diffs.get(&file.path) {
            let runs = diff_view::compute_highlights(&file.path, &diff.hunks);
            self.highlight_cache.insert(file.path.clone(), runs);
        }
    }

    let highlights = if expanded.get() {
        self.highlight_cache.get(&file.path)
    } else {
        None
    };

    // ... render file section with highlights ...
}
```

## Impact

- Initial render: 0 tree-sitter parses instead of N.
- Expanding a file: 1 parse (cached for subsequent frames).
- Collapsing: cached highlights persist, no re-parse on re-expand.
- Highlight cache is already invalidated per-path in `apply_results` when
  diffs change (spec 02), so stale highlights are impossible.
