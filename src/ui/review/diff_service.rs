use super::changes_tab::FileStatus;
use super::diff_engine::{self, DiffStats, FileDiff};
use shuru_sdk::AsyncSandbox;
use std::sync::Arc;

/// Manages sandbox references and async diff/file operations for the review panel.
#[derive(Clone)]
pub struct DiffService {
    sandbox: Arc<AsyncSandbox>,
    host_mount_path: Option<String>,
    tokio_handle: tokio::runtime::Handle,
}

impl DiffService {
    pub fn new(
        sandbox: Arc<AsyncSandbox>,
        host_mount_path: Option<String>,
        tokio_handle: tokio::runtime::Handle,
    ) -> Self {
        Self { sandbox, host_mount_path, tokio_handle }
    }

    /// Spawn a fire-and-forget async task. Clones self so the future owns the service.
    pub fn spawn<F, Fut>(&self, f: F)
    where
        F: FnOnce(DiffService) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let s = self.clone();
        self.tokio_handle.spawn(async move { f(s).await });
    }

    /// Spawn an async task that returns a value.
    pub fn spawn_result<F, Fut, T>(&self, f: F) -> tokio::task::JoinHandle<T>
    where
        F: FnOnce(DiffService) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let s = self.clone();
        self.tokio_handle.spawn(async move { f(s).await })
    }

    /// Compute a file diff. Call from a spawned task.
    pub async fn compute_diff(&self, path: &str) -> FileDiff {
        let old = match self.host_mount_path.as_deref() {
            Some(host) => diff_engine::read_host_file(path, host).await,
            None => None,
        };
        let new = diff_engine::read_sandbox_file(path, "/workspace", &self.sandbox).await;
        diff_engine::compute_file_diff(
            &old.unwrap_or_default(),
            &new.unwrap_or_default(),
        )
    }

    /// Compute just the additions/deletions for a file without building the
    /// hunk/line structures. Cheap enough to run eagerly for every changed
    /// file so the header totals stay accurate; expand still triggers the
    /// full `compute_diff` lazily.
    pub async fn compute_stats(&self, path: &str) -> (DiffStats, bool) {
        let old = match self.host_mount_path.as_deref() {
            Some(host) => diff_engine::read_host_file(path, host).await,
            None => None,
        };
        let new = diff_engine::read_sandbox_file(path, "/workspace", &self.sandbox).await;
        diff_engine::compute_file_stats(
            &old.unwrap_or_default(),
            &new.unwrap_or_default(),
        )
    }

    /// Keep a file change — copy to host or delete host file.
    pub async fn keep_file(&self, path: &str, status: FileStatus) {
        match status {
            FileStatus::Deleted => {
                if let Some(ref host) = self.host_mount_path {
                    let hp = format!("{}/{}", host, path);
                    let _ = tokio::fs::remove_file(&hp).await;
                }
            }
            _ => {
                if let Some(ref host) = self.host_mount_path {
                    let _ = diff_engine::copy_to_host(path, host, &self.sandbox).await;
                }
            }
        }
    }

    /// Discard overlay changes for a file in the sandbox.
    pub async fn discard_file(&self, path: &str) {
        let full = format!("/workspace/{}", path);
        let _ = self.sandbox.discard_overlay(&full).await;
    }
}
