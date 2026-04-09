//! Shuru runtime image download & verification.
//!
//! Mirrors the logic from `shuru-cli/src/assets.rs`: downloads a single
//! tar.gz from GitHub Releases containing kernel, rootfs, and initramfs,
//! then streams it to disk.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;

/// The shuru release version we need. Must match a published GitHub release.
const SHURU_VERSION: &str = "0.5.5";

/// Expected files inside the tar.gz.
const REQUIRED_FILES: &[&str] = &["Image", "rootfs.ext4", "initramfs.cpio.gz"];

/// Default data directory: `~/.local/share/shuru`
pub fn data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".local/share/shuru")
}

/// Check if the runtime is already installed and matches our version.
pub fn is_ready() -> bool {
    let dir = data_dir();
    for name in REQUIRED_FILES {
        if !dir.join(name).exists() {
            return false;
        }
    }
    let version_path = dir.join("VERSION");
    match std::fs::read_to_string(&version_path) {
        Ok(v) => v.trim() == SHURU_VERSION,
        Err(_) => false,
    }
}

/// Download and extract the shuru OS image. Calls `on_progress` during download,
/// then `on_extracting` when switching to extraction, then `on_done` when finished.
pub async fn download_and_install(
    on_progress: impl Fn(u64, Option<u64>) + Send + 'static,
    on_extracting: impl Fn() + Send + 'static,
    on_done: impl Fn(Result<()>) + Send + 'static,
) {
    let result = tokio::task::spawn_blocking(move || {
        do_download(&on_progress, &on_extracting)
    })
    .await;

    match result {
        Ok(Ok(())) => on_done(Ok(())),
        Ok(Err(e)) => on_done(Err(e)),
        Err(e) => on_done(Err(anyhow::anyhow!("task panicked: {e}"))),
    }
}

fn do_download(
    on_progress: &dyn Fn(u64, Option<u64>),
    on_extracting: &dyn Fn(),
) -> Result<()> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create {}", dir.display()))?;

    let tag = format!("v{SHURU_VERSION}");
    let url = format!(
        "https://github.com/superhq-ai/shuru/releases/download/{tag}/shuru-os-{tag}-aarch64.tar.gz"
    );

    // Blocking HTTP GET with streaming
    let response = reqwest::blocking::Client::new()
        .get(&url)
        .send()
        .with_context(|| format!("download failed — is version {tag} released?"))?;

    if !response.status().is_success() {
        bail!(
            "download failed: HTTP {} for {url}",
            response.status()
        );
    }

    let total = response.content_length();

    // Stream through progress tracker → gzip → tar → disk
    // Note: download and extraction are interleaved (streaming through gzip),
    // so we fire on_extracting when the last byte is read, not upfront.
    let reader = ProgressReader {
        inner: response,
        downloaded: 0,
        on_progress,
        on_extracting: Some(on_extracting),
        total,
    };


    let decoder = flate2::read::GzDecoder::new(reader);
    let mut archive = tar::Archive::new(decoder);
    archive
        .unpack(&dir)
        .context("failed to extract shuru runtime archive")?;

    // Write VERSION marker
    std::fs::write(dir.join("VERSION"), SHURU_VERSION)
        .context("failed to write VERSION file")?;

    // Verify all files present
    for name in REQUIRED_FILES {
        if !dir.join(name).exists() {
            bail!("extraction succeeded but {name} is missing from archive");
        }
    }

    Ok(())
}

/// Wraps a reader and reports download progress.
/// Fires `on_extracting` once when the download completes (reader returns 0 bytes).
struct ProgressReader<'a, R> {
    inner: R,
    downloaded: u64,
    on_progress: &'a dyn Fn(u64, Option<u64>),
    on_extracting: Option<&'a dyn Fn()>,
    total: Option<u64>,
}

impl<R: std::io::Read> std::io::Read for ProgressReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.downloaded += n as u64;
        (self.on_progress)(self.downloaded, self.total);
        if n == 0 {
            if let Some(cb) = self.on_extracting.take() {
                cb();
            }
        }
        Ok(n)
    }
}
