# 04 — Reduce per-frame allocations in render + prepaint

## Problem

The render loop and DiffBlock prepaint allocate heavily on every frame:

### A. Path cloned 3x per file per frame (changes_tab.rs:222-245)

```rust
let ss = self.scroll_states.entry(file.path.clone()).or_insert_with(...);  // clone 1
let expanded = self.expanded.entry(file.path.clone()).or_insert_with(...); // clone 2
let path = file.path.clone();  // clone 3
// then cloned again into on_keep and on_discard closures
```

### B. DiffBlock clones all lines + highlights on creation (diff_view.rs:275-287)

```rust
Self {
    lines: collect_lines(hunks),       // clones every line content String
    highlights: highlights.cloned(),   // clones entire HighlightCache
    ...
}
```

`collect_lines` (diff_view.rs:38-65) allocates a new `DiffDisplayLine` per
line with `line.content.trim_end_matches('\n').to_string()`.

### C. Font cloned per line in prepaint (diff_view.rs:361, 387, 192, 204, 217)

`mono.clone()` on every gutter line and every content line. Font contains a
`SharedString` family name — the clone is cheap but needless at this volume.

### D. Content string cloned in shape_line (diff_view.rs:395)

```rust
SharedString::from(line.content.clone())
```

## Fixes

### A. Use entry API with references where possible

The `entry()` calls need owned keys. Switch `scroll_states`, `expanded`, and
`highlight_cache` to key on an index (usize) or use `get()` first:

```rust
// Try get first (no clone), only clone on insert
let ss = self.scroll_states.get(&file.path)
    .cloned()
    .unwrap_or_else(|| {
        let s = DiffScrollState::new();
        self.scroll_states.insert(file.path.clone(), s.clone());
        s
    });
```

Or simpler: pre-populate these maps in `apply_results` when files arrive,
then `render()` just does `get()`.

### B. Store DiffDisplayLines in ChangesTab, not DiffBlock

Pre-compute `collect_lines()` once in `apply_results` (when diff data
arrives), store alongside the FileDiff. DiffBlock takes `&[DiffDisplayLine]`
by reference instead of owning cloned lines.

Same for highlights: DiffBlock takes `Option<&HighlightCache>` (it already
does via the parameter, but then calls `.cloned()` — just borrow it through
prepaint instead).

This requires DiffBlock to hold references, which means it can't be an owned
Element. Alternative: wrap lines and highlights in `Arc` so cloning is free.

### C. Clone font once, reuse

```rust
let mono = font(theme.mono_font_family.clone());
// Use &mono everywhere in prepaint — the TextRun needs owned Font,
// but we can't avoid that without changing gpui's API.
```

Font clones are actually cheap (SharedString is Arc-backed). This is low
priority — fix only if profiling shows it matters after A and B.

### D. Store content as SharedString in DiffDisplayLine

Change `content: String` to `content: SharedString` in DiffDisplayLine.
Then `shape_line` can take it directly without cloning:

```rust
SharedString::from(line.content.clone())  // before — allocates
line.content.clone()                       // after — Arc bump
```

## Priority

A and B are the biggest wins. C and D are diminishing returns — do them
only if the panel still feels slow after A+B.
