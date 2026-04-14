# Changelog

## 0.3.4

- Codex installer works again. Matches shuru 0.5.9 which drops the guest's silent-first-component stripping in favor of an explicit `strip_components` per download. Flat tarballs (Codex) and directory-wrapped ones (Node, Pi) now extract correctly.
- Bumped shuru runtime to 0.5.9.

## 0.3.3

- Appearance settings: theme picker with Light, Dark, Washi, Sumi, and Auto (follows system). Hot-swap — chrome, terminals, and scrollbars update in place without restart.
- Terminal palette, scrollbar, and agent icons adopt the active theme. Pi icon inverts on light themes instead of disappearing into the background.
- COLORFGBG hint passed to the guest and host shells so TUIs pick a matching light/dark palette.
- Runtime download is reliable on re-install: tar.gz is downloaded to a temp file first, then extracted, instead of streaming through gzip+tar in one pass. Progress bar no longer freezes on the rootfs write.
- Sandbox uses self-contained checkpoints (Direct mode) so a shuru version bump doesn't invalidate previously-saved workspaces.
- Terminal view no longer slips under the Ports status bar.
- Opening the Ports dialog while a terminal is still setting up no longer panics.
- App icon now renders in release builds (was referencing the source tree's absolute path).

## 0.3.2

- Review panel is a lot faster with many changed files. Each row is its own view, so hover no longer re-renders the full list every frame.
- Header totals (+N/-M) show up eagerly without computing the full hunk diff for every file.
- Deletions in mounted workspaces now show up reliably, including the case where a file only exists on the host.
- Discard on a deletion no longer flickers.
- Preserve the review panel's accumulated changes when switching to a tab without a sandbox instead of wiping it.

## 0.3.1

- Setting to disable auto-launch of the default agent on workspace open.
- New workspaces activate automatically on creation.
- About tab in settings.
- Settings content scrolls properly when it overflows.
- Shortcuts list regrouped to match the website.

## 0.3.0

- Clickable URLs in the terminal (cmd+click to open).
- Sidebar scrolls with a visible scrollbar.
- Settings moved to the titlebar.
- Review panel hidden on the host terminal.
- Keyboard badges suppressed while dialogs are open.

## 0.2.9

- Host terminal tab with a local PTY for host-side tasks.
- Collapsible sidebar.
- Workspace switch toast.
- Ports disabled on the host terminal.
- Fixed bracketed paste display garbling caused by readline's CR-only redisplay.

## 0.2.8

- Select and copy from the diff view.
- Logo centering fixes and OG image.

## 0.2.7

- Collapsible dock.
- Custom titlebar.
- Keyboard shortcuts with focus management.
- Ports shortcut.
- Enhanced superhq-dark theme.
- Extracted theme and syntax colors to JSON.

## 0.2.6

- Agent lifecycle hooks.
- Per-sandbox review watchers.
- OpenAI gateway fixes.
- Fixed re-entrant terminal panel borrow that crashed on opening settings.

## 0.2.4

- Agent notifications.
- Codex OAuth support for the Pi coding agent.
- Fixed missing keybinding hint for deactivated workspaces.
