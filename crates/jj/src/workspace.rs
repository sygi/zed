use anyhow::{Result, anyhow};
use jj_lib::backend::{ChangeId, CommitId};
use jj_lib::commit::Commit;
use jj_lib::config::StackedConfig;
use jj_lib::conflicts::{ConflictMarkerStyle, MaterializedTreeValue, materialize_tree_value};
use jj_lib::ref_name::WorkspaceNameBuf;
use jj_lib::repo::{ReadonlyRepo, Repo as _, RepoLoader, StoreFactories};
use jj_lib::repo_path::RepoPath;
use jj_lib::settings::UserSettings;
use jj_lib::transaction::Transaction;
use jj_lib::working_copy::CheckoutOptions;
use jj_lib::workspace::{self, DefaultWorkspaceLoaderFactory, WorkspaceLoaderFactory};
use log::{debug, warn};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Thin wrapper around `jj_lib`'s workspace APIs for UI consumers.
pub struct JjWorkspace {
    repo_loader: RepoLoader,
    workspace_name: WorkspaceNameBuf,
    workspace_root: PathBuf,
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
            workspace_root: workspace.workspace_root().to_path_buf(),
        })
    }

    fn load_workspace(&self) -> Result<workspace::Workspace> {
        let loader = DefaultWorkspaceLoaderFactory.create(&self.workspace_root)?;
        let config = StackedConfig::with_defaults();
        let settings = UserSettings::from_config(config)?;
        Ok(loader.load(
            &settings,
            &StoreFactories::default(),
            &workspace::default_working_copy_factories(),
        )?)
    }

    fn load_workspace_and_repo(&self) -> Result<(workspace::Workspace, Arc<ReadonlyRepo>)> {
        let workspace = self.load_workspace()?;
        let repo = workspace.repo_loader().load_at_head()?;
        Ok((workspace, repo))
    }

    fn resolve_change_commit(repo: &Arc<ReadonlyRepo>, change_id: &ChangeId) -> Result<Commit> {
        let Some(commit_ids) = repo.resolve_change_id(change_id) else {
            return Err(anyhow!("change {} not found", short_change_hash(change_id)));
        };
        let commit_id = commit_ids.first().ok_or_else(|| {
            anyhow!(
                "change {} has no associated commits",
                short_change_hash(change_id)
            )
        })?;
        Ok(repo.store().get_commit(commit_id)?)
    }

    fn apply_transaction(
        &self,
        workspace: &mut workspace::Workspace,
        mut tx: Transaction,
        description: impl Into<String>,
    ) -> Result<()> {
        tx.repo_mut().rebase_descendants()?;
        let old_repo = tx.base_repo().clone();
        let new_repo = tx.commit(description)?;

        let workspace_name = workspace.workspace_name().to_owned();
        let old_wc_commit = old_repo
            .view()
            .get_wc_commit_id(&workspace_name)
            .map(|id| old_repo.store().get_commit(id))
            .transpose()?;

        let new_wc_commit_id = new_repo
            .view()
            .get_wc_commit_id(&workspace_name)
            .ok_or_else(|| {
                anyhow!(
                    "workspace '{}' missing working copy commit",
                    workspace_name.as_str()
                )
            })?;
        let new_wc_commit = new_repo.store().get_commit(new_wc_commit_id)?;

        let old_tree = old_wc_commit
            .as_ref()
            .map(|commit| commit.tree_id().clone());
        workspace.check_out(
            new_repo.op_id().clone(),
            old_tree.as_ref(),
            &new_wc_commit,
            &CheckoutOptions {
                conflict_marker_style: ConflictMarkerStyle::default(),
            },
        )?;
        Ok(())
    }

    pub fn edit_change(&self, change_id: &ChangeId) -> Result<()> {
        let (mut workspace, repo) = self.load_workspace_and_repo()?;
        let commit = Self::resolve_change_commit(&repo, change_id)?;
        let mut tx = repo.start_transaction();
        tx.repo_mut()
            .edit(workspace.workspace_name().to_owned(), &commit)?;
        self.apply_transaction(
            &mut workspace,
            tx,
            format!("edit change {}", short_change_hash(change_id)),
        )
    }

    pub fn rename_change(&self, change_id: &ChangeId, new_description: &str) -> Result<()> {
        let (mut workspace, repo) = self.load_workspace_and_repo()?;
        let commit = Self::resolve_change_commit(&repo, change_id)?;
        let mut tx = repo.start_transaction();
        {
            let builder = tx.repo_mut().rewrite_commit(&commit);
            let builder = builder.set_description(new_description.to_string());
            builder.write()?;
        }
        self.apply_transaction(
            &mut workspace,
            tx,
            format!("rename change {}", short_change_hash(change_id)),
        )
    }

    pub async fn parent_tree_text(&self, path: &RepoPath) -> Result<Option<String>> {
        debug!(
            target: "jj::workspace",
            "parent_tree_text requested: workspace={} path={}",
            self.workspace_name.as_str(),
            path.as_internal_file_string()
        );
        let repo = self.repo_loader.load_at_head()?;
        let Some(wc_commit_id) = repo.view().get_wc_commit_id(&self.workspace_name) else {
            warn!(
                target: "jj::workspace",
                "missing working copy commit: workspace={} path={}",
                self.workspace_name.as_str(),
                path.as_internal_file_string()
            );
            return Ok(None);
        };
        debug!(
            target: "jj::workspace",
            "materializing parent tree: workspace={} path={} commit={:?}",
            self.workspace_name.as_str(),
            path.as_internal_file_string(),
            wc_commit_id
        );
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

        let text = bytes.and_then(|data| String::from_utf8(data).ok());
        debug!(
            target: "jj::workspace",
            "parent_tree_text resolved: workspace={} path={} bytes={}",
            self.workspace_name.as_str(),
            path.as_internal_file_string(),
            text.as_ref().map(|t| t.len()).unwrap_or(0)
        );
        Ok(text)
    }

    pub fn current_change_id(&self) -> Result<Option<ChangeId>> {
        let repo = self.repo_loader.load_at_head()?;
        let Some(wc_commit_id) = repo.view().get_wc_commit_id(&self.workspace_name) else {
            return Ok(None);
        };
        let commit = repo.store().get_commit(wc_commit_id)?;
        Ok(Some(commit.change_id().clone()))
    }

    pub fn recent_commits(&self, limit: usize) -> Result<Vec<CommitSummary>> {
        let repo = self.repo_loader.load_at_head()?;
        let store = repo.store();
        let mut heads: Vec<_> = repo.view().heads().iter().cloned().collect();
        heads.sort();
        let mut stack = Vec::new();
        for head in heads {
            let commit = store.get_commit(&head)?;
            stack.push(commit);
        }

        let mut visited = HashSet::new();
        let mut summaries = Vec::new();

        while let Some(commit) = stack.pop() {
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

            let mut parents: Vec<_> = commit.parent_ids().iter().cloned().collect();
            parents.reverse();
            for parent_id in parents {
                let parent = store.get_commit(&parent_id)?;
                stack.push(parent);
            }
        }

        Ok(summaries)
    }
}

pub fn short_change_hash(change_id: &ChangeId) -> String {
    format!("{change_id:.12}")
}

pub fn short_commit_hash(commit_id: &CommitId) -> String {
    format!("{commit_id:.12}")
}
