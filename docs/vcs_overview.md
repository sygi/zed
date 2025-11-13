# VCS Architecture Notes

## Changes Introduced

### 1. Scoped JJ Repository Tracking (`crates/jj/src/tracker.rs`, `crates/worktree/src/worktree.rs`)
- **What**: Added a dedicated tracker module inside the `jj` crate that owns `JjTracker`, `JjRepositoryEntry`, and the `UpdatedJjRepository` payloads. The worktree scanner now imports this tracker behind the `jj-ui` feature flag, so `.jj` metadata is monitored only when the flag is enabled.
- **Why**: Keeps jj-specific logic isolated from the generic worktree pipeline and makes it easy to plug in future backends without sprinkling jj structs across unrelated modules. Moving tracking into its own crate also lets us evolve jj integration independently of Git.

### 2. Feature-flagged JJ Scanning (`crates/feature_flags/src/flags.rs`, `crates/worktree/src/worktree.rs`)
- **What**: Added `JjUiFeatureFlag`; the worktree scanner, `WorktreeStore`, and downstream events emit `UpdatedJjRepositories` only when the flag is on.
- **Why**: Prevents jj logic from running for users who do not need it and allows us to experiment safely. The Git behavior is unchanged when the flag is off.

### 3. VCS Facade in `project` (`crates/project/src/vcs.rs`, `crates/project/src/project.rs`)
- **What**: Introduced a `VcsBackend` trait plus a `GitVcsBackend` implementation that wraps the existing `GitStore`. `Project` now holds an `Arc<dyn VcsBackend>` and delegates diff/permalink/blame/status queries through the trait.
- **Why**: This creates the seam needed for future backends (jj or others) without disturbing existing Git code. Behavior is identical today because the only backend is Git, but the project layer is no longer hardwired to Git types.

### 4. JJ Panel Prototype (`crates/jj_ui/src/lib.rs`, `crates/zed/src/zed.rs`)
- **What**: Added a feature-gated JJ panel module in the `jj_ui` crate that registers a `JjPanel` dock. The panel fetches the latest JJ commits on demand via `JjStore`/`jj-lib` and renders a simple history list with a refresh button.
- **Why**: Gives us a visible surface for JJ work without touching the Git UI. Pulling the change graph on open keeps the initial implementation simple while we iterate on backend caching.

## Exposed VCS Operations

The current abstraction covers the “read” surface area needed by the editor: 

| Operation | Description | Git Support | JJ Support (current) |
|-----------|-------------|-------------|----------------------|
| `open_unstaged_diff` | Show working tree vs index/parent. | Implements via `GitStore::open_unstaged_diff`. | Delegates to Git backend for now; jj will map this to “current change vs parent” when a jj backend exists. |
| `open_uncommitted_diff` | Show index vs HEAD. | Implements via `GitStore::open_uncommitted_diff`. | Same fallback as above until a jj backend is ready. |
| `blame_buffer` | Inline blame / annotate. | Uses Git blame API. | Will invoke `jj annotate` via a jj backend; today still Git-only. |
| `get_permalink_to_line` | Generate stable URL for line. | Uses Git commit info. | JJ backend would generate change IDs; currently Git-only. |
| Repository metadata (`active_repository`, `repositories`) | Enumerate repositories known to the project. | Directly backed by `GitStore`. | JJ tracker already publishes repo descriptors but no backend consumes them yet; Git backend handles all repos for now. |
| `status_for_buffer_id` | File status badges / inline diff gutters. | Uses cached Git status. | JJ backend will eventually translate jj change state to generic `FileStatus`; currently Git-only. |

Write-side operations (stage, unstage, commit, branch, push/pull, init, etc.) still call into `GitStore` directly. Once we introduce a jj backend we can extend `VcsBackend` with the operations that make sense for multiple systems or default to the Git implementation where jj does not apply.

## Next Steps (not yet implemented)
1. Expand `VcsBackend` with capability reporting so UI can hide Git-specific chrome when a backend doesn’t support indexes/remotes.
2. Add a `JjVcsBackend` that consumes the data from the new tracker helpers and implements at least the shared read APIs by calling `jj file diff` / `jj annotate` (or future jj RPCs).
3. Route write operations (stage/commit/etc.) through the facade where they make semantic sense, falling back to Git-only wiring otherwise.


TODO: remove the 1-2 part, import jj-lib always like before but hide it behind a flag, get a facade for a simple feature and implement it for jj. Then implement a jj panel instead of a git one.
Keep JJ core helpers in `crates/jj` and UI pieces in `crates/jj_ui` while wiring them through the generic VCS facade.

## Running With JJ UI Enabled
- Build/launch Zed with the `jj-ui` feature to exercise the new panel and VCS hooks:
  ```bash
  RUST_LOG=error,jj::diff=info,jj::workspace=debug \
  cargo run -p zed --features jj-ui
  ```
- Feature flags default to “on” in debug builds, so the JJ panel appears for any workspace containing a `.jj` directory once the feature is enabled at compile time.
