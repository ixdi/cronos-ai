use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Condvar, Mutex, MutexGuard, PoisonError},
    thread::{self, ThreadId},
};

use serde::{Serialize, de::DeserializeOwned};
use tempfile::NamedTempFile;
use thiserror::Error;
use uuid::Uuid;

use crate::model::{ModelError, Project, ProjectId, Task, TaskId};

const MAX_JSON_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("storage I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error("record not found: {0}")]
    NotFound(PathBuf),
    #[error("refusing unsafe storage path: {0}")]
    UnsafePath(PathBuf),
    #[error("JSON file exceeds the 1 MiB limit: {0}")]
    TooLarge(PathBuf),
    #[error("stored project ID does not match its path")]
    ProjectIdMismatch,
    #[error("stored task ID does not match its path")]
    TaskIdMismatch,
    #[error("stored task does not belong to the requested project")]
    TaskProjectMismatch,
    #[error("project does not list the requested task")]
    TaskNotRegistered,
    #[error("project directory has an invalid ID: {0}")]
    InvalidProjectDirectory(PathBuf),
}

pub struct ProjectStore {
    root: PathBuf,
    write_lock: Mutex<WriteLockState>,
    write_lock_released: Condvar,
}

/// Who holds the store write lock in this process. The file lock excludes other processes; the
/// owner and depth make it reentrant for the holding thread and exclusive across threads.
#[derive(Default)]
struct WriteLockState {
    owner: Option<ThreadId>,
    depth: usize,
    file: Option<File>,
}

/// Exclusive right to change the store, held until dropped. Every read-check-save of projects and
/// tasks runs under it so concurrent changes never overwrite each other. The operating system
/// releases the file lock when the holder exits, so a crash never leaves the store locked.
#[must_use = "the store is unlocked as soon as the guard is dropped"]
pub struct StoreWriteLock<'a> {
    store: &'a ProjectStore,
}

impl Drop for StoreWriteLock<'_> {
    fn drop(&mut self) {
        let mut state = self.store.write_lock_state();
        state.depth -= 1;
        if state.depth == 0 {
            state.owner = None;
            // Closing the file releases the operating-system lock.
            state.file = None;
            self.store.write_lock_released.notify_one();
        }
    }
}

impl ProjectStore {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn expected_task_worktree_path(
        &self,
        project_id: ProjectId,
        task_id: TaskId,
        attempt: u32,
    ) -> PathBuf {
        self.root
            .join("worktrees")
            .join(project_id.0.to_string())
            .join(task_id.0.to_string())
            .join(format!("attempt-{attempt}"))
    }

    pub fn worktrees_root(&self) -> PathBuf {
        self.root.join("worktrees")
    }

    pub fn new(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        fs::create_dir_all(root.as_ref())?;
        let root = fs::canonicalize(root)?;
        if !root.is_dir() {
            return Err(StoreError::UnsafePath(root));
        }
        Ok(Self {
            root,
            write_lock: Mutex::default(),
            write_lock_released: Condvar::new(),
        })
    }

    /// Waits for the exclusive store write lock. A thread that already holds it gets it again
    /// immediately, so composite operations can nest.
    pub fn lock_writes(&self) -> Result<StoreWriteLock<'_>, StoreError> {
        let current = thread::current().id();
        let mut state = self.write_lock_state();
        loop {
            match state.owner {
                None => break,
                Some(owner) if owner == current => {
                    state.depth += 1;
                    return Ok(StoreWriteLock { store: self });
                }
                Some(_) => {
                    state = self
                        .write_lock_released
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            }
        }
        let directory = self.checked_directory(&self.root.join("locks"), true)?;
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(directory.join("store.lock"))?;
        file.lock()?;
        *state = WriteLockState {
            owner: Some(current),
            depth: 1,
            file: Some(file),
        };
        Ok(StoreWriteLock { store: self })
    }

    fn write_lock_state(&self) -> MutexGuard<'_, WriteLockState> {
        self.write_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub fn save_project(&self, project: &Project) -> Result<(), StoreError> {
        project.validate()?;
        let directory = self.project_directory(project.id, true)?;
        let path = directory.join("project.json");

        if regular_file_exists(&path)? {
            let existing: Project = read_json(&path)?;
            existing.validate()?;
            if existing.id != project.id {
                return Err(StoreError::ProjectIdMismatch);
            }
        }

        write_json_atomically(&path, project)
    }

    pub fn load_project(&self, id: ProjectId) -> Result<Project, StoreError> {
        let path = self.project_directory(id, false)?.join("project.json");
        let project: Project = read_json(&path)?;
        project.validate()?;
        if project.id != id {
            return Err(StoreError::ProjectIdMismatch);
        }
        Ok(project)
    }

    pub fn list_projects(&self) -> Result<Vec<Project>, StoreError> {
        let directory = match self.checked_directory(&self.root.join("projects"), false) {
            Ok(directory) => directory,
            Err(StoreError::NotFound(_)) => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut projects = Vec::new();
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                return Err(StoreError::UnsafePath(entry.path()));
            }
            if !file_type.is_dir() {
                continue;
            }
            let id = entry
                .file_name()
                .to_str()
                .and_then(|name| Uuid::parse_str(name).ok())
                .map(ProjectId)
                .ok_or_else(|| StoreError::InvalidProjectDirectory(entry.path()))?;
            projects.push(self.load_project(id)?);
        }
        Ok(projects)
    }

    pub fn save_task(&self, task: &Task) -> Result<(), StoreError> {
        task.validate()?;
        let project = self.load_project(task.project_id)?;
        if project.id != task.project_id {
            return Err(StoreError::TaskProjectMismatch);
        }

        let directory = self.task_directory(task.project_id, true)?;
        let path = directory.join(format!("{}.json", task.id.0));
        if regular_file_exists(&path)? {
            let existing: Task = read_json(&path)?;
            existing.validate()?;
            if existing.id != task.id {
                return Err(StoreError::TaskIdMismatch);
            }
            if existing.project_id != task.project_id {
                return Err(StoreError::TaskProjectMismatch);
            }
        }

        write_json_atomically(&path, task)
    }

    pub fn load_task(&self, project_id: ProjectId, task_id: TaskId) -> Result<Task, StoreError> {
        let project = self.load_project(project_id)?;
        if !project.task_ids.contains(&task_id) {
            return Err(StoreError::TaskNotRegistered);
        }

        let path = self
            .task_directory(project_id, false)?
            .join(format!("{}.json", task_id.0));
        let task: Task = read_json(&path)?;
        task.validate()?;
        if task.id != task_id {
            return Err(StoreError::TaskIdMismatch);
        }
        if task.project_id != project_id {
            return Err(StoreError::TaskProjectMismatch);
        }
        Ok(task)
    }

    pub fn list_tasks(&self, project_id: ProjectId) -> Result<Vec<Task>, StoreError> {
        let project = self.load_project(project_id)?;
        project
            .task_ids
            .into_iter()
            .map(|task_id| self.load_task(project_id, task_id))
            .collect()
    }

    fn project_directory(&self, id: ProjectId, create: bool) -> Result<PathBuf, StoreError> {
        let projects = self.checked_directory(&self.root.join("projects"), create)?;
        self.checked_directory(&projects.join(id.0.to_string()), create)
    }

    fn task_directory(&self, project_id: ProjectId, create: bool) -> Result<PathBuf, StoreError> {
        let project = self.project_directory(project_id, false)?;
        self.checked_directory(&project.join("tasks"), create)
    }

    fn checked_directory(&self, path: &Path, create: bool) -> Result<PathBuf, StoreError> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(StoreError::UnsafePath(path.to_path_buf()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
                fs::create_dir(path)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::NotFound(path.to_path_buf()));
            }
            Err(error) => return Err(error.into()),
        }

        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(StoreError::UnsafePath(path.to_path_buf()));
        }
        let resolved = fs::canonicalize(path)?;
        if resolved == self.root || !resolved.starts_with(&self.root) {
            return Err(StoreError::UnsafePath(path.to_path_buf()));
        }
        Ok(resolved)
    }
}

fn regular_file_exists(path: &Path) -> Result<bool, StoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(StoreError::UnsafePath(path.to_path_buf()))
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, StoreError> {
    if !regular_file_exists(path)? {
        return Err(StoreError::NotFound(path.to_path_buf()));
    }

    let file = File::open(path)?;
    if file.metadata()?.len() > MAX_JSON_BYTES {
        return Err(StoreError::TooLarge(path.to_path_buf()));
    }
    let mut bytes = Vec::new();
    file.take(MAX_JSON_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_JSON_BYTES {
        return Err(StoreError::TooLarge(path.to_path_buf()));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn write_json_atomically<T: Serialize>(path: &Path, value: &T) -> Result<(), StoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| StoreError::UnsafePath(path.to_path_buf()))?;
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(&bytes)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| StoreError::Io(error.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ProjectStore, StoreError};
    use crate::model::{Priority, Project, ProjectId, ProjectStatus, Task, TaskId, TaskStatus};
    use std::collections::BTreeMap;

    fn project(task_ids: Vec<TaskId>) -> Project {
        Project {
            id: ProjectId::new(),
            name: "Cronos project".to_owned(),
            description: Some("Test project".to_owned()),
            status: ProjectStatus::Active,
            task_ids,
            repository: None,
            check_command: None,
            check_timeout_minutes: None,
            publish: Default::default(),
            remote: None,
            releases: false,
            requirements: Vec::new(),
            created_at_unix_secs: 1,
            updated_at_unix_secs: 1,
        }
    }

    fn task(project_id: ProjectId, id: TaskId) -> Task {
        Task {
            id,
            project_id,
            title: "Implement persistence".to_owned(),
            description: None,
            priority: Priority::High,
            dependencies: Vec::new(),
            status: TaskStatus::Pending,
            current_step: None,
            step_started_at_unix_secs: BTreeMap::new(),
            agent_activity: Vec::new(),
            approval_history: Vec::new(),
            abandonment_note: None,
            worktree: None,
            attempt: 1,
            recovery_history: Vec::new(),
            automatic_revisions: 0,
            published: None,
            release: None,
            requirement_ids: Vec::new(),
            created_at_unix_secs: 2,
            updated_at_unix_secs: 2,
        }
    }

    #[test]
    fn project_and_task_survive_store_reopen() {
        let root = tempfile::tempdir().unwrap();
        let task_id = TaskId::new();
        let project = project(vec![task_id]);
        let task = task(project.id, task_id);

        let store = ProjectStore::new(root.path()).unwrap();
        store.save_project(&project).unwrap();
        store.save_task(&task).unwrap();
        drop(store);

        let reopened = ProjectStore::new(root.path()).unwrap();
        assert_eq!(reopened.load_project(project.id).unwrap(), project);
        assert_eq!(reopened.load_task(project.id, task_id).unwrap(), task);
    }

    fn lock_file_is_held(root: &std::path::Path) -> bool {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(root.join("locks").join("store.lock"))
            .unwrap();
        match file.try_lock() {
            Ok(()) => false,
            Err(std::fs::TryLockError::WouldBlock) => true,
            Err(std::fs::TryLockError::Error(error)) => panic!("{error}"),
        }
    }

    #[test]
    fn write_lock_is_reentrant_and_held_until_the_outermost_guard_drops() {
        let root = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(root.path()).unwrap();

        let outer = store.lock_writes().unwrap();
        let inner = store.lock_writes().unwrap();
        drop(inner);
        assert!(lock_file_is_held(root.path()));
        drop(outer);
        // A process another test forks at this moment shares the lock until it execs, so the
        // release can take a few milliseconds to become visible.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while lock_file_is_held(root.path()) {
            assert!(
                std::time::Instant::now() < deadline,
                "the lock stayed held after the outermost guard dropped"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn write_lock_excludes_other_threads() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let root = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(root.path()).unwrap();
        let acquired = AtomicBool::new(false);

        let guard = store.lock_writes().unwrap();
        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                let _guard = store.lock_writes().unwrap();
                acquired.store(true, Ordering::SeqCst);
            });
            std::thread::sleep(std::time::Duration::from_millis(100));
            assert!(!acquired.load(Ordering::SeqCst));
            drop(guard);
            waiter.join().unwrap();
        });
        assert!(acquired.load(Ordering::SeqCst));
    }

    #[test]
    fn invalid_existing_project_is_not_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(root.path()).unwrap();
        let original = project(Vec::new());
        store.save_project(&original).unwrap();

        let path = root
            .path()
            .join("projects")
            .join(original.id.0.to_string())
            .join("project.json");
        let corrupt = b"{ invalid json";
        std::fs::write(&path, corrupt).unwrap();

        let mut replacement = original.clone();
        replacement.name = "Replacement".to_owned();
        assert!(matches!(
            store.save_project(&replacement),
            Err(StoreError::Json(_))
        ));
        assert_eq!(std::fs::read(path).unwrap(), corrupt);
    }

    #[test]
    fn project_symlink_is_rejected() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let store = ProjectStore::new(root.path()).unwrap();
            std::fs::create_dir(root.path().join("projects")).unwrap();
            let project = project(Vec::new());
            symlink(
                outside.path(),
                root.path().join("projects").join(project.id.0.to_string()),
            )
            .unwrap();

            assert!(matches!(
                store.save_project(&project),
                Err(StoreError::UnsafePath(_))
            ));
            assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
        }
    }
}
