use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::Path;
use std::sync::Arc;

use util::rel_path::RelPath;

#[derive(Clone, Debug)]
pub struct JjRepositoryEntry<ID> {
    pub work_directory_id: ID,
    pub work_directory_abs_path: Arc<Path>,
    pub work_directory_rel_path: Arc<RelPath>,
    pub jj_dir_abs_path: Arc<Path>,
    pub jj_dir_scan_id: usize,
    pub covers_entire_project: bool,
}

impl<ID> JjRepositoryEntry<ID> {
    pub fn directory_contains(&self, path: &RelPath) -> bool {
        self.covers_entire_project || path.starts_with(self.work_directory_rel_path.as_ref())
    }
}

#[derive(Clone, Debug)]
pub struct UpdatedJjRepository<ID> {
    pub work_directory_id: ID,
    pub old_work_directory_abs_path: Option<Arc<Path>>,
    pub new_work_directory_abs_path: Option<Arc<Path>>,
    pub jj_dir_abs_path: Option<Arc<Path>>,
}

pub type UpdatedJjRepositoriesSet<ID> = Arc<[UpdatedJjRepository<ID>]>;

#[derive(Clone)]
pub struct JjTracker<ID> {
    enabled: bool,
    repositories: BTreeMap<ID, JjRepositoryEntry<ID>>,
}

impl<ID> JjTracker<ID>
where
    ID: Ord + Copy + Clone,
{
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            repositories: BTreeMap::new(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn should_force_scan(&self, file_name: &OsStr) -> bool {
        self.enabled && file_name == ".jj"
    }

    pub fn repositories(&self) -> &BTreeMap<ID, JjRepositoryEntry<ID>> {
        &self.repositories
    }

    pub fn repositories_mut(&mut self) -> &mut BTreeMap<ID, JjRepositoryEntry<ID>> {
        &mut self.repositories
    }

    pub fn insert(&mut self, entry: JjRepositoryEntry<ID>) {
        self.repositories.insert(entry.work_directory_id, entry);
    }

    pub fn mark_scan(&mut self, id: &ID, scan_id: usize) {
        if let Some(repo) = self.repositories.get_mut(id) {
            repo.jj_dir_scan_id = scan_id;
        }
    }

    pub fn repo_for_relative_path(&self, path: &RelPath) -> Option<(&ID, &JjRepositoryEntry<ID>)> {
        self.repositories
            .iter()
            .find(|(_, repo)| repo.work_directory_rel_path.as_ref() == path)
    }

    pub fn repo_id_by_jj_dir(&self, jj_dir: &Path) -> Option<ID> {
        self.repositories.iter().find_map(|(id, repo)| {
            if repo.jj_dir_abs_path.as_ref() == jj_dir {
                Some(*id)
            } else {
                None
            }
        })
    }

    pub fn repos_containing<'a>(
        &'a self,
        path: &'a RelPath,
    ) -> impl Iterator<Item = (&'a ID, &'a JjRepositoryEntry<ID>)> + 'a {
        self.repositories
            .iter()
            .filter(move |(_, repo)| repo.directory_contains(path))
    }

    pub fn retain_existing<F>(&mut self, mut exists: F)
    where
        F: FnMut(&JjRepositoryEntry<ID>) -> bool,
    {
        self.repositories.retain(|_, repo| exists(repo));
    }

    pub fn diff(
        old: &BTreeMap<ID, JjRepositoryEntry<ID>>,
        new: &BTreeMap<ID, JjRepositoryEntry<ID>>,
    ) -> UpdatedJjRepositoriesSet<ID> {
        let mut changes = Vec::new();

        let mut old_iter = old.iter().peekable();
        let mut new_iter = new.iter().peekable();

        loop {
            match (new_iter.peek(), old_iter.peek()) {
                (Some((new_id, new_repo)), Some((old_id, old_repo))) => match new_id.cmp(old_id) {
                    std::cmp::Ordering::Less => {
                        changes.push(UpdatedJjRepository {
                            work_directory_id: **new_id,
                            old_work_directory_abs_path: None,
                            new_work_directory_abs_path: Some(
                                new_repo.work_directory_abs_path.clone(),
                            ),
                            jj_dir_abs_path: Some(new_repo.jj_dir_abs_path.clone()),
                        });
                        new_iter.next();
                    }
                    std::cmp::Ordering::Equal => {
                        if new_repo.jj_dir_scan_id != old_repo.jj_dir_scan_id
                            || new_repo.work_directory_abs_path != old_repo.work_directory_abs_path
                        {
                            changes.push(UpdatedJjRepository {
                                work_directory_id: **new_id,
                                old_work_directory_abs_path: Some(
                                    old_repo.work_directory_abs_path.clone(),
                                ),
                                new_work_directory_abs_path: Some(
                                    new_repo.work_directory_abs_path.clone(),
                                ),
                                jj_dir_abs_path: Some(new_repo.jj_dir_abs_path.clone()),
                            });
                        }
                        new_iter.next();
                        old_iter.next();
                    }
                    std::cmp::Ordering::Greater => {
                        changes.push(UpdatedJjRepository {
                            work_directory_id: **old_id,
                            old_work_directory_abs_path: Some(
                                old_repo.work_directory_abs_path.clone(),
                            ),
                            new_work_directory_abs_path: None,
                            jj_dir_abs_path: Some(old_repo.jj_dir_abs_path.clone()),
                        });
                        old_iter.next();
                    }
                },
                (Some((new_id, new_repo)), None) => {
                    changes.push(UpdatedJjRepository {
                        work_directory_id: **new_id,
                        old_work_directory_abs_path: None,
                        new_work_directory_abs_path: Some(new_repo.work_directory_abs_path.clone()),
                        jj_dir_abs_path: Some(new_repo.jj_dir_abs_path.clone()),
                    });
                    new_iter.next();
                }
                (None, Some((old_id, old_repo))) => {
                    changes.push(UpdatedJjRepository {
                        work_directory_id: **old_id,
                        old_work_directory_abs_path: Some(old_repo.work_directory_abs_path.clone()),
                        new_work_directory_abs_path: None,
                        jj_dir_abs_path: Some(old_repo.jj_dir_abs_path.clone()),
                    });
                    old_iter.next();
                }
                (None, None) => break,
            }
        }

        changes.into()
    }
}
