use crate::Project;
use crate::git_store::{GitStore, Repository, RepositoryId};
#[cfg(feature = "jj-ui")]
use crate::jj_store::JjStore;
use anyhow::Result;
use buffer_diff::BufferDiff;
use collections::HashMap;
#[cfg(feature = "jj-ui")]
use feature_flags::{FeatureFlagAppExt as _, JjUiFeatureFlag};
use git::blame::Blame;
use git::status::FileStatus;
use gpui::{App, Context, Entity, Task};
use language::Buffer;
use std::ops::Range;
use text::BufferId;
use url::Url;

pub trait VcsBackend: Send + Sync + 'static {
    fn open_unstaged_diff(
        &self,
        buffer: Entity<Buffer>,
        cx: &mut Context<Project>,
    ) -> Task<Result<Entity<BufferDiff>>>;

    fn open_uncommitted_diff(
        &self,
        buffer: Entity<Buffer>,
        cx: &mut Context<Project>,
    ) -> Task<Result<Entity<BufferDiff>>>;

    fn blame_buffer(
        &self,
        buffer: &Entity<Buffer>,
        version: Option<clock::Global>,
        cx: &mut App,
    ) -> Task<Result<Option<Blame>>>;

    fn get_permalink_to_line(
        &self,
        buffer: &Entity<Buffer>,
        selection: Range<u32>,
        cx: &mut App,
    ) -> Task<Result<Url>>;

    fn active_repository(&self, cx: &App) -> Option<Entity<Repository>>;

    fn repositories<'a>(&'a self, cx: &'a App) -> &'a HashMap<RepositoryId, Entity<Repository>>;

    fn status_for_buffer_id(&self, buffer_id: BufferId, cx: &App) -> Option<FileStatus>;

    fn recalculate_buffer_diffs(
        &self,
        buffers: Vec<Entity<Buffer>>,
        cx: &mut Context<Project>,
    ) -> Task<()>;
}

pub struct ProjectVcsBackend {
    git: GitVcsBackend,
    #[cfg(feature = "jj-ui")]
    jj: Option<JjVcsBackend>,
}

impl ProjectVcsBackend {
    #[cfg(feature = "jj-ui")]
    pub fn new(git_store: Entity<GitStore>, jj_store: Option<Entity<JjStore>>) -> Self {
        Self {
            git: GitVcsBackend::new(git_store),
            jj: jj_store.map(JjVcsBackend::new),
        }
    }

    #[cfg(not(feature = "jj-ui"))]
    pub fn new(git_store: Entity<GitStore>) -> Self {
        Self {
            git: GitVcsBackend::new(git_store),
        }
    }
}

impl VcsBackend for ProjectVcsBackend {
    fn open_unstaged_diff(
        &self,
        buffer: Entity<Buffer>,
        cx: &mut Context<Project>,
    ) -> Task<Result<Entity<BufferDiff>>> {
        #[cfg(feature = "jj-ui")]
        {
            if let Some(jj) = self.preferred_jj_backend(cx) {
                // TODO: allow users to configure the preferred VCS priority once settings exist.
                if let Some(task) = jj.open_unstaged_diff(buffer.clone(), cx) {
                    return task;
                }
            }
        }
        self.git.open_unstaged_diff(buffer, cx)
    }

    fn open_uncommitted_diff(
        &self,
        buffer: Entity<Buffer>,
        cx: &mut Context<Project>,
    ) -> Task<Result<Entity<BufferDiff>>> {
        #[cfg(feature = "jj-ui")]
        {
            if let Some(jj) = self.preferred_jj_backend(cx) {
                if let Some(task) = jj.open_uncommitted_diff(buffer.clone(), cx) {
                    return task;
                }
            }
        }
        self.git.open_uncommitted_diff(buffer, cx)
    }

    fn blame_buffer(
        &self,
        buffer: &Entity<Buffer>,
        version: Option<clock::Global>,
        cx: &mut App,
    ) -> Task<Result<Option<Blame>>> {
        self.git.blame_buffer(buffer, version, cx)
    }

    fn get_permalink_to_line(
        &self,
        buffer: &Entity<Buffer>,
        selection: Range<u32>,
        cx: &mut App,
    ) -> Task<Result<Url>> {
        self.git.get_permalink_to_line(buffer, selection, cx)
    }

    fn active_repository(&self, cx: &App) -> Option<Entity<Repository>> {
        self.git.active_repository(cx)
    }

    fn repositories<'a>(&'a self, cx: &'a App) -> &'a HashMap<RepositoryId, Entity<Repository>> {
        self.git.repositories(cx)
    }

    fn status_for_buffer_id(&self, buffer_id: BufferId, cx: &App) -> Option<FileStatus> {
        self.git.status_for_buffer_id(buffer_id, cx)
    }

    fn recalculate_buffer_diffs(
        &self,
        buffers: Vec<Entity<Buffer>>,
        cx: &mut Context<Project>,
    ) -> Task<()> {
        #[cfg(feature = "jj-ui")]
        {
            if let Some(jj) = self.preferred_jj_backend(cx) {
                return jj.recalculate_buffer_diffs(buffers, cx);
            }
        }
        self.git.recalculate_buffer_diffs(buffers, cx)
    }
}

pub struct GitVcsBackend {
    git_store: Entity<GitStore>,
}

impl GitVcsBackend {
    pub fn new(git_store: Entity<GitStore>) -> Self {
        Self { git_store }
    }
}

#[cfg(feature = "jj-ui")]
struct JjVcsBackend {
    jj_store: Entity<JjStore>,
}

#[cfg(feature = "jj-ui")]
impl JjVcsBackend {
    fn new(jj_store: Entity<JjStore>) -> Self {
        Self { jj_store }
    }

    fn open_unstaged_diff(
        &self,
        buffer: Entity<Buffer>,
        cx: &mut Context<Project>,
    ) -> Option<Task<Result<Entity<BufferDiff>>>> {
        self.jj_store
            .update(cx, |store, cx| store.open_unstaged_diff(buffer.clone(), cx))
    }

    fn open_uncommitted_diff(
        &self,
        buffer: Entity<Buffer>,
        cx: &mut Context<Project>,
    ) -> Option<Task<Result<Entity<BufferDiff>>>> {
        self.jj_store.update(cx, |store, cx| {
            store.open_uncommitted_diff(buffer.clone(), cx)
        })
    }

    fn has_repositories(&self, cx: &mut Context<Project>) -> bool {
        self.jj_store.read(cx).has_repositories()
    }

    fn recalculate_buffer_diffs(
        &self,
        buffers: Vec<Entity<Buffer>>,
        cx: &mut Context<Project>,
    ) -> Task<()> {
        match self
            .jj_store
            .update(cx, |store, cx| store.recalculate_buffer_diffs(buffers, cx))
        {
            Some(task) => task,
            None => Task::ready(()),
        }
    }
}

#[cfg(feature = "jj-ui")]
impl ProjectVcsBackend {
    fn preferred_jj_backend<'a>(&'a self, cx: &mut Context<Project>) -> Option<&'a JjVcsBackend> {
        let jj = self.jj.as_ref()?;
        if !cx.has_flag::<JjUiFeatureFlag>() {
            return None;
        }
        if !jj.has_repositories(cx) {
            return None;
        }
        Some(jj)
    }
}

impl VcsBackend for GitVcsBackend {
    fn open_unstaged_diff(
        &self,
        buffer: Entity<Buffer>,
        cx: &mut Context<Project>,
    ) -> Task<Result<Entity<BufferDiff>>> {
        self.git_store
            .update(cx, |git_store, cx| git_store.open_unstaged_diff(buffer, cx))
    }

    fn open_uncommitted_diff(
        &self,
        buffer: Entity<Buffer>,
        cx: &mut Context<Project>,
    ) -> Task<Result<Entity<BufferDiff>>> {
        self.git_store.update(cx, |git_store, cx| {
            git_store.open_uncommitted_diff(buffer, cx)
        })
    }

    fn blame_buffer(
        &self,
        buffer: &Entity<Buffer>,
        version: Option<clock::Global>,
        cx: &mut App,
    ) -> Task<Result<Option<Blame>>> {
        self.git_store.update(cx, |git_store, cx| {
            git_store.blame_buffer(buffer, version, cx)
        })
    }

    fn get_permalink_to_line(
        &self,
        buffer: &Entity<Buffer>,
        selection: Range<u32>,
        cx: &mut App,
    ) -> Task<Result<Url>> {
        self.git_store.update(cx, |git_store, cx| {
            git_store.get_permalink_to_line(buffer, selection, cx)
        })
    }

    fn active_repository(&self, cx: &App) -> Option<Entity<Repository>> {
        self.git_store.read(cx).active_repository()
    }

    fn repositories<'a>(&'a self, cx: &'a App) -> &'a HashMap<RepositoryId, Entity<Repository>> {
        self.git_store.read(cx).repositories()
    }

    fn status_for_buffer_id(&self, buffer_id: BufferId, cx: &App) -> Option<FileStatus> {
        self.git_store.read(cx).status_for_buffer_id(buffer_id, cx)
    }

    fn recalculate_buffer_diffs(
        &self,
        buffers: Vec<Entity<Buffer>>,
        cx: &mut Context<Project>,
    ) -> Task<()> {
        let future = self.git_store.update(cx, |git_store, cx| {
            git_store.recalculate_buffer_diffs(buffers, cx)
        });
        cx.background_executor().spawn(async move {
            future.await;
        })
    }
}
