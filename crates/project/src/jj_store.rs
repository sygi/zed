use crate::worktree_store::{WorktreeStore, WorktreeStoreEvent};
use anyhow::Result;
use buffer_diff::BufferDiff;
#[cfg(feature = "jj-ui")]
use gpui::SharedString;
use gpui::{AppContext as _, AsyncApp, Context, Entity, Subscription, Task, WeakEntity};
use jj::{ChangeId, CommitId, CommitSummary, JjWorkspace, RepoPathBuf, short_change_hash};
use language::{Buffer, LocalFile};
use log::{debug, info, warn};
use parking_lot::Mutex;
use std::{collections::HashMap, path::Path, sync::Arc};
use text::BufferId;
use worktree::{JjRepoEntryForWorktree, ProjectEntryId, Worktree, WorktreeId};

pub struct JjStore {
    worktree_store: Entity<WorktreeStore>,
    repositories_by_worktree: HashMap<WorktreeId, Vec<Arc<JjRepositoryState>>>,
    repositories_by_id: HashMap<ProjectEntryId, Arc<JjRepositoryState>>,
    diffs_by_buffer: HashMap<BufferId, JjDiffState>,
    _subscriptions: Vec<Subscription>,
}

#[cfg(feature = "jj-ui")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JjCommitSummary {
    pub commit_id: CommitId,
    pub change_id: ChangeId,
    pub description: SharedString,
    pub author: SharedString,
    pub timestamp: i64,
}

#[cfg(feature = "jj-ui")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JjRepositorySummary {
    pub id: ProjectEntryId,
    pub worktree_id: WorktreeId,
    pub path: SharedString,
}

impl JjStore {
    pub fn new(worktree_store: Entity<WorktreeStore>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            worktree_store: worktree_store.clone(),
            repositories_by_worktree: HashMap::new(),
            repositories_by_id: HashMap::new(),
            diffs_by_buffer: HashMap::new(),
            _subscriptions: Vec::new(),
        };

        this.refresh_existing_worktrees(cx);
        this._subscriptions
            .push(cx.subscribe(&worktree_store, Self::on_worktree_store_event));
        this
    }

    pub fn open_unstaged_diff(
        &mut self,
        buffer: Entity<Buffer>,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<Entity<BufferDiff>>>> {
        let (repository, repo_path) = self.repository_and_path_for_buffer(&buffer, cx)?;
        let workspace = match repository.workspace() {
            Ok(workspace) => workspace,
            Err(err) => {
                warn!(
                    target: "jj::diff",
                    "failed to load jj workspace for {}: {err:?}",
                    repository
                        .work_directory_path()
                        .display()
                        .to_string()
                );
                return Some(Task::ready(Err(err)));
            }
        };
        let repo_root = repository.work_directory_path();
        let repo_root_display = repo_root.display().to_string();
        let repo_path_string = repo_path.as_internal_file_string().to_owned();
        info!(
            target: "jj::diff",
            "open_unstaged_diff requested: repo_root={} path={}",
            repo_root_display,
            repo_path_string
        );

        let (buffer_id, language, language_registry, text_snapshot) = {
            let buffer_guard = buffer.read(cx);
            (
                buffer_guard.remote_id(),
                buffer_guard.language().cloned(),
                buffer_guard.language_registry(),
                buffer_guard.text_snapshot(),
            )
        };

        let diff = cx.new(|cx| BufferDiff::new(&text_snapshot, cx));
        let repo_path_for_task = repo_path.clone();
        let repo_root_display_for_task = repo_root_display.clone();
        let repo_path_string_for_task = repo_path_string.clone();
        let store = cx.entity().downgrade();
        let repository_for_task = repository.clone();
        let task = cx.spawn(async move |_, cx| {
            debug!(
                target: "jj::diff",
                "materializing parent tree text: repo_root={} path={}",
                repo_root_display_for_task,
                repo_path_string_for_task
            );
            let base_text = match workspace
                .parent_tree_text(repo_path_for_task.as_ref())
                .await
            {
                Ok(text) => {
                    info!(
                        target: "jj::diff",
                        "parent tree ready: repo_root={} path={} bytes={}",
                        repo_root_display_for_task,
                        repo_path_string_for_task,
                        text.as_ref().map(|t| t.len()).unwrap_or(0)
                    );
                    text
                }
                Err(err) => {
                    warn!(
                        target: "jj::diff",
                        "failed to materialize parent tree text: repo_root={} path={} err={:?}",
                        repo_root_display_for_task,
                        repo_path_string_for_task,
                        err
                    );
                    return Err(err);
                }
            };
            let base_text = base_text.map(Arc::new);
            let rx = diff.update(cx, |diff, cx| {
                diff.set_base_text(
                    base_text.clone(),
                    language.clone(),
                    language_registry.clone(),
                    text_snapshot.clone(),
                    cx,
                )
            })?;
            rx.await?;
            if let Some(store) = store.upgrade() {
                store
                    .update(cx, |store, _| {
                        store.track_diff(
                            buffer_id,
                            diff.downgrade(),
                            repository_for_task.clone(),
                            repo_path_for_task.clone(),
                        );
                    })
                    .ok();
            }
            Ok(diff)
        });

        Some(task)
    }

    pub fn open_uncommitted_diff(
        &mut self,
        buffer: Entity<Buffer>,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<Entity<BufferDiff>>>> {
        self.open_unstaged_diff(buffer, cx)
    }

    pub fn recalculate_buffer_diffs(
        &mut self,
        buffers: Vec<Entity<Buffer>>,
        cx: &mut Context<Self>,
    ) -> Option<Task<()>> {
        let mut jobs = Vec::new();
        for buffer in buffers {
            let (buffer_id, state) = {
                let buffer_id = buffer.read(cx).remote_id();
                let state = self.diffs_by_buffer.get(&buffer_id).cloned();
                (buffer_id, state)
            };
            if let Some(state) = state {
                jobs.push((buffer, buffer_id, state));
            }
        }
        if jobs.is_empty() {
            return None;
        }
        let store = cx.entity().downgrade();
        Some(cx.spawn(async move |_, cx| {
            for (buffer, buffer_id, state) in jobs {
                if let Err(err) =
                    Self::recalculate_diff_for_job(&store, buffer.clone(), buffer_id, state, cx)
                        .await
                {
                    warn!(
                        target: "jj::diff",
                        "failed to recalc diff for buffer {buffer_id:?}: {err:?}"
                    );
                }
            }
        }))
    }

    fn repository_and_path_for_buffer(
        &self,
        buffer: &Entity<Buffer>,
        cx: &Context<Self>,
    ) -> Option<(Arc<JjRepositoryState>, RepoPathBuf)> {
        let (worktree_id, abs_path) = {
            let buffer = buffer.read(cx);
            let file = worktree::File::from_dyn(buffer.file())?;
            if !file.is_local {
                return None;
            }
            (file.worktree_id(cx), file.abs_path(cx))
        };

        let repositories = self.repositories_by_worktree.get(&worktree_id)?;
        repositories.iter().find_map(|repo| {
            repo.relative_repo_path(&abs_path)
                .map(|path| (repo.clone(), path))
        })
    }

    fn refresh_existing_worktrees(&mut self, cx: &mut Context<Self>) {
        let store = self.worktree_store.read(cx);
        for worktree in store.worktrees() {
            self.add_worktree_repositories(&worktree, cx);
        }
    }

    fn add_worktree_repositories(&mut self, worktree: &Entity<Worktree>, cx: &Context<Self>) {
        let (worktree_id, entries) = {
            let guard = worktree.read(cx);
            (guard.id(), guard.jj_repository_entries())
        };
        if let Some(entries) = entries {
            for entry in entries {
                self.track_repository(worktree_id, entry);
            }
        }
    }

    fn on_worktree_store_event(
        &mut self,
        _: Entity<WorktreeStore>,
        event: &WorktreeStoreEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            WorktreeStoreEvent::WorktreeAdded(worktree) => {
                self.add_worktree_repositories(worktree, cx)
            }
            WorktreeStoreEvent::WorktreeRemoved(_, worktree_id)
            | WorktreeStoreEvent::WorktreeReleased(_, worktree_id) => {
                self.remove_worktree(*worktree_id)
            }
            WorktreeStoreEvent::WorktreeUpdatedJjRepositories(worktree_id, changes) => {
                let worktree = self
                    .worktree_store
                    .read(cx)
                    .worktree_for_id(*worktree_id, cx);

                for change in changes.iter() {
                    if change.new_work_directory_abs_path.is_some() {
                        if let Some(worktree) = worktree.clone() {
                            if let Some(entry) = worktree
                                .read(cx)
                                .jj_repository_entry(change.work_directory_id)
                            {
                                self.track_repository(*worktree_id, entry);
                            }
                        }
                    } else {
                        self.remove_repository(change.work_directory_id);
                    }
                }
            }
            _ => {}
        }
    }

    fn track_repository(&mut self, worktree_id: WorktreeId, entry: JjRepoEntryForWorktree) {
        let state = Arc::new(JjRepositoryState::from_entry(worktree_id, entry));
        self.repositories_by_id
            .insert(state.work_directory_id, state.clone());
        let repos = self
            .repositories_by_worktree
            .entry(worktree_id)
            .or_default();
        repos.push(state);
        repos.sort_by(|a, b| b.path_depth.cmp(&a.path_depth));
    }

    fn remove_repository(&mut self, work_directory_id: ProjectEntryId) {
        if let Some(state) = self.repositories_by_id.remove(&work_directory_id) {
            if let Some(repos) = self.repositories_by_worktree.get_mut(&state.worktree_id) {
                repos.retain(|repo| repo.work_directory_id != work_directory_id);
                if repos.is_empty() {
                    self.repositories_by_worktree.remove(&state.worktree_id);
                }
            }
        }
    }

    fn track_diff(
        &mut self,
        buffer_id: BufferId,
        diff: WeakEntity<BufferDiff>,
        repository: Arc<JjRepositoryState>,
        repo_path: RepoPathBuf,
    ) {
        self.diffs_by_buffer.insert(
            buffer_id,
            JjDiffState {
                diff,
                repository,
                repo_path,
            },
        );
    }

    async fn recalculate_diff_for_job(
        store: &WeakEntity<Self>,
        buffer: Entity<Buffer>,
        buffer_id: BufferId,
        state: JjDiffState,
        cx: &mut AsyncApp,
    ) -> Result<()> {
        let Some(diff_entity) = state.diff.upgrade() else {
            if let Some(store) = store.upgrade() {
                store
                    .update(cx, |store, _| {
                        store.diffs_by_buffer.remove(&buffer_id);
                    })
                    .ok();
            }
            return Ok(());
        };

        let workspace = state.repository.workspace()?;
        let repo_path = state.repo_path.clone();
        let repo_root = state.repository.work_directory_path();
        let repo_root_display = repo_root.display().to_string();
        let path_string = repo_path.as_internal_file_string().to_owned();
        debug!(
            target: "jj::diff",
            "recalculating diff base: repo_root={} path={}",
            repo_root_display,
            path_string
        );

        let base_text = workspace.parent_tree_text(repo_path.as_ref()).await?;
        let base_text = base_text.map(Arc::new);
        let (language, language_registry, text_snapshot) = buffer.read_with(cx, |buffer, _| {
            (
                buffer.language().cloned(),
                buffer.language_registry(),
                buffer.text_snapshot(),
            )
        })?;

        let rx = diff_entity.update(cx, |diff, cx| {
            diff.set_base_text(
                base_text.clone(),
                language.clone(),
                language_registry.clone(),
                text_snapshot.clone(),
                cx,
            )
        })?;
        rx.await?;
        Ok(())
    }

    fn remove_worktree(&mut self, worktree_id: WorktreeId) {
        if let Some(repos) = self.repositories_by_worktree.remove(&worktree_id) {
            for repo in repos {
                self.repositories_by_id.remove(&repo.work_directory_id);
            }
        }
    }

    #[cfg(feature = "jj-ui")]
    pub fn has_repositories(&self) -> bool {
        !self.repositories_by_id.is_empty()
    }

    #[cfg(feature = "jj-ui")]
    pub fn repositories(&self) -> Vec<JjRepositorySummary> {
        self.repositories_by_id
            .values()
            .map(|repo| JjRepositorySummary {
                id: repo.work_directory_id,
                worktree_id: repo.worktree_id,
                path: SharedString::from(repo.display_name()),
            })
            .collect()
    }

    #[cfg(feature = "jj-ui")]
    pub fn recent_commits(
        &mut self,
        repository_id: Option<ProjectEntryId>,
        limit: usize,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<Vec<JjCommitSummary>>>> {
        let repo = match repository_id {
            Some(id) => self.repositories_by_id.get(&id)?.clone(),
            None => self.repositories_by_id.values().next()?.clone(),
        };
        let task = cx.background_spawn(async move {
            let workspace = repo.workspace()?;
            let current_change = workspace.current_change_id()?;
            let commits = workspace.recent_commits(limit)?;
            let summaries = commits
                .into_iter()
                .map(|summary| {
                    let is_current = current_change
                        .as_ref()
                        .is_some_and(|id| id == summary.change_id());
                    JjCommitSummary {
                        commit_id: summary.commit_id,
                        change_id: summary.change_id,
                        description: SharedString::from(summary.description),
                        author: SharedString::from(summary.author),
                        timestamp: summary.timestamp,
                        is_current,
                    }
                })
                .collect();
            Ok(summaries)
        });
        Some(task)
    }

    #[cfg(feature = "jj-ui")]
    pub fn edit_change(
        &mut self,
        repository_id: ProjectEntryId,
        change_id: ChangeId,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<()>>> {
        let repository = self.repositories_by_id.get(&repository_id)?.clone();
        Some(cx.spawn(async move |_, _| {
            repository.workspace()?.edit_change(&change_id)?;
            info!(
                target: "project::jj_store",
                "switched workspace {:?} to change {}",
                repository_id,
                short_change_hash(&change_id)
            );
            Ok(())
        }))
    }

    #[cfg(feature = "jj-ui")]
    pub fn rename_change(
        &mut self,
        repository_id: ProjectEntryId,
        change_id: ChangeId,
        new_description: String,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<()>>> {
        let repository = self.repositories_by_id.get(&repository_id)?.clone();
        Some(cx.spawn(async move |_, _| {
            repository
                .workspace()?
                .rename_change(&change_id, &new_description)?;
            info!(
                target: "project::jj_store",
                "renamed change {} in repo {:?}",
                short_change_hash(&change_id),
                repository_id
            );
            Ok(())
        }))
    }
}

#[derive(Clone)]
struct JjDiffState {
    diff: WeakEntity<BufferDiff>,
    repository: Arc<JjRepositoryState>,
    repo_path: RepoPathBuf,
}

struct JjRepositoryState {
    worktree_id: WorktreeId,
    work_directory_id: ProjectEntryId,
    work_directory_abs_path: Arc<Path>,
    path_depth: usize,
    workspace: Mutex<Option<Arc<JjWorkspace>>>,
}

impl JjRepositoryState {
    fn from_entry(worktree_id: WorktreeId, entry: JjRepoEntryForWorktree) -> Self {
        let path_depth = entry.work_directory_abs_path.components().count();
        Self {
            worktree_id,
            work_directory_id: entry.work_directory_id,
            work_directory_abs_path: entry.work_directory_abs_path.clone(),
            path_depth,
            workspace: Mutex::new(None),
        }
    }

    fn workspace(&self) -> Result<Arc<JjWorkspace>> {
        let mut cached = self.workspace.lock();
        if let Some(workspace) = cached.as_ref() {
            return Ok(workspace.clone());
        }
        let workspace = Arc::new(JjWorkspace::load(self.work_directory_abs_path.as_ref())?);
        *cached = Some(workspace.clone());
        Ok(workspace)
    }

    fn relative_repo_path(&self, file_abs_path: &Path) -> Option<RepoPathBuf> {
        let relative = file_abs_path
            .strip_prefix(self.work_directory_abs_path.as_ref())
            .ok()?;
        RepoPathBuf::from_relative_path(relative).ok()
    }

    fn work_directory_path(&self) -> Arc<Path> {
        self.work_directory_abs_path.clone()
    }

    #[cfg(feature = "jj-ui")]
    fn display_name(&self) -> String {
        self.work_directory_abs_path.display().to_string()
    }
}
