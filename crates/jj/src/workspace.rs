use anyhow::{Result, anyhow};
use jj_lib::config::StackedConfig;
use jj_lib::conflicts::{MaterializedTreeValue, materialize_tree_value};
use jj_lib::ref_name::WorkspaceNameBuf;
use jj_lib::repo::{Repo as _, RepoLoader, StoreFactories};
use jj_lib::repo_path::RepoPath;
use jj_lib::settings::UserSettings;
use jj_lib::workspace::{self, DefaultWorkspaceLoaderFactory, WorkspaceLoaderFactory};
use std::path::Path;

/// Thin wrapper around `jj_lib`'s workspace APIs for UI consumers.
pub struct JjWorkspace {
    repo_loader: RepoLoader,
    workspace_name: WorkspaceNameBuf,
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
}
