use anyhow::{Result, anyhow};
use jj_lib::backend::{ChangeId, CommitId};
use jj_lib::config::StackedConfig;
use jj_lib::conflicts::{MaterializedTreeValue, materialize_tree_value};
use jj_lib::ref_name::WorkspaceNameBuf;
use jj_lib::repo::{Repo as _, RepoLoader, StoreFactories};
use jj_lib::repo_path::RepoPath;
use jj_lib::settings::UserSettings;
use jj_lib::workspace::{self, DefaultWorkspaceLoaderFactory, WorkspaceLoaderFactory};
use std::collections::{HashSet, VecDeque};
use std::path::Path;

/// Thin wrapper around `jj_lib`'s workspace APIs for UI consumers.
pub struct JjWorkspace {
    repo_loader: RepoLoader,
    workspace_name: WorkspaceNameBuf,
}

#[derive(Debug, Clone)]
pub struct CommitSummary {
    pub commit_id: CommitId,
    pub change_id: ChangeId,
    pub author: String,
    pub description: String,
    pub timestamp: i64,
}

impl JjWorkspace {
    pub fn load(workspace_root: impl AsRef<Path>) -> Result<Self> {
        let workspace_root = workspace_root.as_ref();
        let loader = DefaultWorkspaceLoaderFactory.create(workspace_root)?;
        let config = StackedConfig::with_defaults();
        let settings = UserSettings::from_config(config)?;
        let workspace = loader.load(
            &settings,
            &StoreFactories::default(),
            &workspace::default_working_copy_factories(),
        )?;

        Ok(Self {
            repo_loader: workspace.repo_loader().clone(),
            workspace_name: workspace.workspace_name().to_owned(),
        })
    }

    pub async fn parent_tree_text(&self, path: &RepoPath) -> Result<Option<String>> {
        let repo = self.repo_loader.load_at_head()?;
        let Some(wc_commit_id) = repo.view().get_wc_commit_id(&self.workspace_name) else {
            return Ok(None);
        };
        let wc_commit = repo.store().get_commit(wc_commit_id)?;
        let parent_tree = wc_commit.parent_tree(repo.as_ref())?;
        let merged_value = parent_tree.path_value(path)?;
        let materialized = materialize_tree_value(repo.store(), path, merged_value).await?;
        let bytes = match materialized {
            MaterializedTreeValue::File(mut file) => Some(file.read_all(path)?),
            MaterializedTreeValue::AccessDenied(err) => {
                return Err(anyhow!("access to {path:?} denied: {err}"));
            }
            _ => None,
        };

        Ok(bytes.and_then(|data| String::from_utf8(data).ok()))
    }

    pub fn recent_commits(&self, limit: usize) -> Result<Vec<CommitSummary>> {
        let repo = self.repo_loader.load_at_head()?;
        let store = repo.store();
        let mut pending = VecDeque::new();
        for head in repo.view().heads() {
            let commit = store.get_commit(head)?;
            pending.push_back(commit);
        }

        let mut visited = HashSet::new();
        let mut summaries = Vec::new();

        while let Some(commit) = pending.pop_front() {
            if !visited.insert(commit.id().clone()) {
                continue;
            }

            let timestamp = commit.committer().timestamp.timestamp;
            summaries.push(CommitSummary {
                commit_id: commit.id().clone(),
                change_id: commit.change_id().clone(),
                author: commit.author().name.clone(),
                description: commit.description().to_string(),
                timestamp: timestamp.0,
            });

            if summaries.len() >= limit {
                break;
            }

            for parent_id in commit.parent_ids() {
                let parent = store.get_commit(parent_id)?;
                pending.push_back(parent);
            }
        }

        summaries.sort_by_key(|summary| summary.timestamp);
        summaries.reverse();
        Ok(summaries)
    }
}
