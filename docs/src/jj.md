# JJ Support Overview

- **Feature Flag & Crate Split** – All JJ features are guarded by the `jj-ui` feature. JJ code is split between `crates/jj` (workspace utilities, commit summaries, VCS backend helpers) and `crates/jj_ui` (panel/UI). This keeps JJ dependencies optional unless the flag is enabled.
- **Project Settings & VCS Selection** – `ProjectSettings::preferred_vcs` (backed by `vcs.default`) lets a project choose “git” or “jj” when the flag is on. The workspace menu and panel loader read that setting to decide which panel to mount, defaulting to Git when unset.
- **Unified VCS Backend** – `project::vcs` delegates gutter-diff operations through `VcsBackend`. When JJ repos exist and the feature flag is enabled, `JjVcsBackend` implements `open_unstaged_diff`, `open_uncommitted_diff`, and `recalculate_buffer_diffs` using jj-lib (see `crates/project/src/jj_store.rs`). The current git behavior is unchanged, implementing the same interface.
- **JJ Workspace Utilities** – `crates/jj/src/workspace.rs` wraps jj-lib’s workspace APIs (loading, current change tracking, commit listing) and snapshots the working copy before `edit_change`/`rename_change` so editor changes are preserved.
- **JJ Store Layer** – `project::jj_store` tracks JJ repositories per worktree, exposes `recent_commits`, `edit_change`, `rename_change`, and manages cached buffer-diff state keyed by `BufferId`. It logs operations via `project::jj_store` and `jj::diff`.
- **JJ Panel UI** (`crates/jj_ui/src/lib.rs`):
  - Repository selector with filled/outlined buttons, a commit list showing short hashes/author/timestamp, and highlighting for the current change.
  - Context menu actions: rename revision modal and `jj edit`; both refresh the panel upon completion.
  - Command palette actions (`jj ui: toggle focus`, `jj ui: open diff`) wired to panel focus and diff commands.

## TODOs

1. **Workspace Notifications** – External JJ CLI changes still require manually pressing Refresh. Hook into a workspace tracker or background watcher so the panel updates automatically.
2. **Graph Panel** – Replace the linear history list with a richer JJ change graph view and visual tools for the stack.
3. **Graph Editing Tools** – Add UI affordances for more JJ operations (add, split, abandon, rebase, ...).
4. **Commit Resolution Flow** – Provide a guided commit-resolution / conflict-management UI integrated with the panel.
5. **Panel UI** – Improve usability of the panel: shorter commit names, better handling immutable revisions, ...
