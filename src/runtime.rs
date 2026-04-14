//! Shuru runtime image download & verification.
//!
//! Downloads a tar.gz from GitHub Releases to a temp file, then extracts
//! it to the data directory. Download and extraction are separate phases
//! so the UI can reflect them independently.

use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The shuru release version we need. Must match a published GitHub release.
const SHURU_VERSION: &str = "0.5.8";

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
    let archive_path = dir.join(format!("shuru-os-{tag}.tar.gz.partial"));

    download_to_file(&url, &archive_path, on_progress)
        .with_context(|| format!("failed to download shuru runtime (version {tag})"))?;

    on_extracting();

    let result = extract_archive(&archive_path, &dir);
    let _ = std::fs::remove_file(&archive_path);
    result.context("failed to extract shuru runtime archive")?;

    std::fs::write(dir.join("VERSION"), SHURU_VERSION)
        .context("failed to write VERSION file")?;

    for name in REQUIRED_FILES {
        if !dir.join(name).exists() {
            bail!("extraction succeeded but {name} is missing from archive");
        }
    }

    Ok(())
}

/// Download `url` to `dest`, reporting progress.
fn download_to_file(
    url: &str,
    dest: &Path,
    on_progress: &dyn Fn(u64, Option<u64>),
) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(600))
        .build()
        .context("failed to build HTTP client")?;

    let response = client
        .get(url)
        .send()
        .with_context(|| format!("request failed: {url}"))?;

    if !response.status().is_success() {
        bail!("download failed: HTTP {} for {url}", response.status());
    }

    let total = response.content_length();
    let tmp = File::create(dest)
        .with_context(|| format!("failed to create {}", dest.display()))?;
    let mut writer = BufWriter::with_capacity(1 << 20, tmp);
    let mut reader = response;
    let mut buf = vec![0u8; 64 * 1024];
    let mut downloaded: u64 = 0;
    // Throttle progress notifications so we don't spam the UI thread.
    let mut last_notified: u64 = 0;

    loop {
        let n = reader.read(&mut buf).context("read from HTTP response")?;
        if n == 0 {
            break;
        }
        writer
            .write_all(&buf[..n])
            .with_context(|| format!("write to {}", dest.display()))?;
        downloaded += n as u64;
        if downloaded - last_notified >= 256 * 1024 {
            on_progress(downloaded, total);
            last_notified = downloaded;
        }
    }
    writer.flush().context("flush download buffer")?;
    on_progress(downloaded, total);

    if let Some(t) = total {
        if downloaded < t {
            bail!("download truncated: got {downloaded} of {t} bytes");
        }
    }
    Ok(())
}

/// Extract a tar.gz from `archive_path` into `dest`. Removes any pre-existing
/// REQUIRED_FILES first so we don't fight overwrite-in-place behavior.
fn extract_archive(archive_path: &Path, dest: &Path) -> Result<()> {
    for name in REQUIRED_FILES {
        let p = dest.join(name);
        if p.exists() {
            let _ = std::fs::remove_file(&p);
        }
    }

    let file = File::open(archive_path)
        .with_context(|| format!("open {}", archive_path.display()))?;
    let reader = BufReader::with_capacity(1 << 20, file);
    let decoder = flate2::read::GzDecoder::new(reader);
    let mut archive = tar::Archive::new(decoder);
    archive.unpack(dest)?;
    Ok(())
}
