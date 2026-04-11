# 01 — Extract watcher into its own module

## Problem

`src/ui/review/mod.rs` mixes UI panel state with a 130-line watch bridge
thread (`start_guest_watch`, lines 167-298). The watch loop owns gitignore
filtering, file I/O, diff computation, result caching, and channel plumbing —
none of which is UI concern.

## Current structure

```
mod.rs (SidePanel)
├── build_ignore_matcher()        — gitignore setup
├── SidePanel::start_guest_watch  — spawns bridge thread + GPUI task
│   ├── sandbox.open_watch()      — subscribe to FS events
│   ├── gitignore filtering       — matched_path_or_any_parents
│   ├── read_host_file / read_sandbox_file
│   ├── compute_file_diff         — diff engine call
│   ├── cached_files / cached_diffs  — accumulated state
│   └── tx.send(DiffResult)       — channel to UI
└── _watch_task (rx loop → apply_diff_result)
```

## Proposed structure

New file: `src/ui/review/watcher.rs`

```
watcher.rs
├── BUILTIN_IGNORE_PATTERNS       — moved from mod.rs
├── build_ignore_matcher()        — moved from mod.rs
├── DiffResult                    — moved from mod.rs
├── WatchBridge::new(sandbox, workspace_path, host_mount_path) -> (Self, flume::Receiver<DiffResult>)
│   └── spawns the bridge thread internally
└── WatchBridge (owns the JoinHandle, drops cleanly)
```

```
mod.rs (SidePanel) — after
├── SidePanel::start_watching
│   └── creates WatchBridge, stores rx, spawns GPUI task
└── apply_diff_result (unchanged)
```

## Steps

1. Create `src/ui/review/watcher.rs`.
2. Move `BUILTIN_IGNORE_PATTERNS`, `build_ignore_matcher()`, and `DiffResult`
   into it.
3. Create `WatchBridge` struct that takes `(Arc<AsyncSandbox>, tokio::runtime::Handle, String, Option<String>)` and returns a `flume::Receiver<DiffResult>`.
4. The bridge thread logic (lines 182-268 of current mod.rs) moves into
   `WatchBridge::new` unchanged — it's already a self-contained closure.
5. `SidePanel::start_guest_watch` becomes ~10 lines: create bridge, store
   receiver, spawn GPUI task on rx.
6. Add `mod watcher;` to mod.rs.

## Not in scope

- Changing the DiffResult payload (see spec 02).
- Changing how highlights are computed (see spec 03).
- Any perf changes — this is a pure structural refactor.
