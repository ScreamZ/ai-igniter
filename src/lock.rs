use anyhow::{bail, Context, Result};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// RAII guard representing an exclusive process lock for a specific workspace project.
///
/// Uses OS kernel advisory locking (`std::fs::File::try_lock` available in Rust std),
/// guaranteeing that the lock is immediately and automatically released by the kernel
/// if the process terminates or crashes, completely eliminating stale/orphan locks.
#[derive(Debug)]
pub struct DevLock {
    file: File,
    #[allow(dead_code)]
    path: PathBuf,
}

impl DevLock {
    /// Attempts to acquire an exclusive lock for the given compose project.
    pub fn acquire(project_name: &str) -> Result<Self> {
        Self::acquire_in(&lock_dir(), project_name)
    }

    /// Internal acquisition helper allowing custom lock directories (used for tests).
    pub fn acquire_in(dir: &Path, project_name: &str) -> Result<Self> {
        fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create lock directory '{}'", dir.display()))?;

        let path = dir.join(format!("{}.lock", project_name));
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("Failed to open lock file '{}'", path.display()))?;

        match file.try_lock() {
            Ok(()) => {
                let pid = std::process::id();
                let _ = file.set_len(0);
                let _ = file.seek(SeekFrom::Start(0));
                let _ = writeln!(file, "{}", pid);
                let _ = file.flush();

                Ok(Self { file, path })
            }
            Err(TryLockError::WouldBlock) => {
                let mut content = String::new();
                let _ = file.seek(SeekFrom::Start(0));
                let _ = file.read_to_string(&mut content);
                let pid_str = content.trim();

                if !pid_str.is_empty() {
                    bail!(
                        "An instance of 'ai-igniter dev' is already running for project '{}' (PID: {}). Stop it before starting a new one.",
                        project_name,
                        pid_str
                    );
                } else {
                    bail!(
                        "An instance of 'ai-igniter dev' is already running for project '{}'. Stop it before starting a new one.",
                        project_name
                    );
                }
            }
            Err(TryLockError::Error(e)) => {
                bail!("Failed to acquire lock for project '{}': {}", project_name, e);
            }
        }
    }

    /// Removes the lock file for the given compose project, if it exists.
    pub fn remove(project_name: &str) {
        Self::remove_in(&lock_dir(), project_name);
    }

    /// Internal removal helper allowing custom lock directories (used for tests).
    pub fn remove_in(dir: &Path, project_name: &str) {
        let path = dir.join(format!("{}.lock", project_name));
        let _ = fs::remove_file(path);
    }
}

impl Drop for DevLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn lock_dir() -> PathBuf {
    let base = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .map(|h| h.join(".ai-igniter"))
        .unwrap_or_else(|_| std::env::temp_dir().join("ai-igniter"));
    base.join("locks")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lock_acquire_and_conflict() {
        let dir = std::env::temp_dir().join(format!("ai_igniter_lock_test_{}", std::process::id()));
        let project = "test-project-lock";

        // First lock should succeed
        let lock1 = DevLock::acquire_in(&dir, project).expect("first lock should succeed");

        // Second lock for the same project while lock1 is held should fail
        let lock2_err = DevLock::acquire_in(&dir, project).unwrap_err();
        assert!(
            lock2_err.to_string().contains("already running"),
            "Error was: {lock2_err}"
        );
        assert!(
            lock2_err
                .to_string()
                .contains(&std::process::id().to_string()),
            "Error should include current PID: {lock2_err}"
        );

        // Dropping first lock
        drop(lock1);

        // Now third lock should succeed
        let lock3 = DevLock::acquire_in(&dir, project).expect("lock should succeed after drop");
        drop(lock3);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_stale_lock_recovery() {
        let dir = std::env::temp_dir().join(format!("ai_igniter_stale_test_{}", std::process::id()));
        let project = "test-project-stale";
        fs::create_dir_all(&dir).unwrap();

        // Write a stale file containing an old PID
        let file_path = dir.join(format!("{}.lock", project));
        fs::write(&file_path, "99999999\n").unwrap();

        // Since no process holds the kernel flock on this file, acquire_in must succeed
        let lock = DevLock::acquire_in(&dir, project).expect("should acquire stale lock");

        // Verify the file now contains our current PID
        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content.trim(), std::process::id().to_string());

        drop(lock);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_lock_remove() {
        let dir = std::env::temp_dir().join(format!("ai_igniter_remove_test_{}", std::process::id()));
        let project = "test-project-remove";

        let lock = DevLock::acquire_in(&dir, project).expect("lock should succeed");
        let file_path = dir.join(format!("{}.lock", project));
        assert!(file_path.exists());

        drop(lock);
        DevLock::remove_in(&dir, project);
        assert!(!file_path.exists());

        let _ = fs::remove_dir_all(&dir);
    }
}
