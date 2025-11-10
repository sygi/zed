use crate::worktree_store::{WorktreeStore, WorktreeStoreEvent};
use anyhow::Result;
use buffer_diff::BufferDiff;
use gpui::{AppContext as _, Context, Entity, Subscription, Task};
use jj::{JjWorkspace, RepoPathBuf};
use language::{Buffer, LocalFile};
use parking_lot::Mutex;
use std::{collections::HashMap, path::Path, sync::Arc};
use worktree::{JjRepoEntryForWorktree, ProjectEntryId, Worktree, WorktreeId};

pub struct JjStore {
    worktree_store: Entity<WorktreeStore>,
    repositories_by_worktree: HashMap<WorktreeId, Vec<Arc<JjRepositoryState>>>,
    repositories_by_id: HashMap<ProjectEntryId, Arc<JjRepositoryState>>,
    _subscriptions: Vec<Subscription>,
}

impl JjStore {
    pub fn new(worktree_store: Entity<WorktreeStore>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            worktree_store: worktree_store.clone(),
            repositories_by_worktree: HashMap::new(),
            repositories_by_id: HashMap::new(),
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
            Err(err) => return Some(Task::ready(Err(err))),
        };

        let (language, language_registry, text_snapshot) = {
            let buffer_guard = buffer.read(cx);
            (
                buffer_guard.language().cloned(),
                buffer_guard.language_registry(),
                buffer_guard.text_snapshot(),
            )
        };

        let diff = cx.new(|cx| BufferDiff::new(&text_snapshot, cx));
        let repo_path = repo_path.clone();
        let task = cx.spawn(async move |_, cx| {
            let base_text = workspace.parent_tree_text(repo_path.as_ref()).await?;
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
            Ok(diff)
        });

        Some(task)
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

    fn remove_worktree(&mut self, worktree_id: WorktreeId) {
        if let Some(repos) = self.repositories_by_worktree.remove(&worktree_id) {
            for repo in repos {
                self.repositories_by_id.remove(&repo.work_directory_id);
            }
        }
    }
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
}
