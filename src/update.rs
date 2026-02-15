//! Auto-update mechanism for claude-rlm.
//!
//! Three-phase update: **check** → **stage** → **apply**.
//!
//! - On MCP startup, apply any previously staged update (rename trick).
//! - In the background, check GitHub for a newer release and stage it.
//! - On next startup, the staged binary gets promoted.

use std::path::{Path, PathBuf};

const REPO: &str = "dullfig/claude-rlm";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Apply a previously staged update. Called early in `run_server()`.
///
/// If `<binary>.staged` exists, promotes it via rename:
/// - Windows: rename self → `.old`, rename `.staged` → self
/// - Unix: atomic rename `.staged` → self
///
/// Returns true if an update was applied.
pub fn apply_staged_update() -> bool {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return false,
    };

    let staged = suffixed_path(&exe, ".staged");
    if !staged.exists() {
        return false;
    }

    tracing::info!("Found staged update at {}, applying...", staged.display());

    #[cfg(windows)]
    {
        let old = suffixed_path(&exe, ".old");
        if let Err(e) = std::fs::rename(&exe, &old) {
            tracing::warn!("Failed to rename current binary to .old: {}", e);
            return false;
        }
        if let Err(e) = std::fs::rename(&staged, &exe) {
            tracing::warn!("Failed to rename staged binary: {}", e);
            // Try to restore
            let _ = std::fs::rename(&old, &exe);
            return false;
        }
        tracing::info!("Update applied successfully (v{})", CURRENT_VERSION);
        true
    }

    #[cfg(not(windows))]
    {
        if let Err(e) = std::fs::rename(&staged, &exe) {
            tracing::warn!("Failed to apply staged update: {}", e);
            return false;
        }
        tracing::info!("Update applied successfully");
        true
    }
}

/// Remove the `.old` file from a previous update (best-effort).
pub fn cleanup_old_binary() {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };

    let old = suffixed_path(&exe, ".old");
    if old.exists() {
        match std::fs::remove_file(&old) {
            Ok(()) => tracing::debug!("Removed old binary {}", old.display()),
            Err(e) => tracing::debug!("Could not remove old binary: {} (may still be locked)", e),
        }
    }
}

/// Spawn a background update check. Non-blocking — returns immediately.
pub fn spawn_update_check() {
    if is_auto_update_disabled() {
        tracing::debug!("Auto-update disabled, skipping check");
        return;
    }

    tokio::spawn(async {
        if let Err(e) = check_and_stage_update().await {
            tracing::debug!("Update check failed: {}", e);
        }
    });
}

/// Check GitHub for a newer release and stage the binary if found.
async fn check_and_stage_update() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;

    if should_skip_check(&exe) {
        tracing::debug!("Update check throttled, skipping");
        return Ok(());
    }

    tracing::info!("Checking for updates...");

    let client = reqwest::Client::builder()
        .user_agent(format!("claude-rlm/{}", CURRENT_VERSION))
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    let url = format!("https://api.github.com/repos/{}/releases/latest", REPO);
    let resp = client.get(&url).send().await?;

    if !resp.status().is_success() {
        // Record check even on failure to avoid hammering on 403/404
        record_check(&exe);
        anyhow::bail!("GitHub API returned {}", resp.status());
    }

    let release: GitHubRelease = resp.json().await?;
    let remote_version = release.tag_name.trim_start_matches('v');

    if !is_newer(remote_version, CURRENT_VERSION) {
        tracing::info!("Up to date (v{})", CURRENT_VERSION);
        record_check(&exe);
        return Ok(());
    }

    tracing::info!(
        "New version available: v{} (current: v{})",
        remote_version,
        CURRENT_VERSION
    );

    let target = platform_target();
    if target == "unknown" {
        record_check(&exe);
        anyhow::bail!("No pre-built binary for this platform");
    }

    let asset_name = if cfg!(windows) {
        format!("claude-rlm-{}.zip", target)
    } else {
        format!("claude-rlm-{}.tar.gz", target)
    };

    let asset = release
        .assets
        .iter()
        .find(|a| a.name == asset_name)
        .ok_or_else(|| anyhow::anyhow!("No asset found for {}", asset_name))?;

    tracing::info!("Downloading {}...", asset.name);
    let resp = client.get(&asset.browser_download_url).send().await?;
    if !resp.status().is_success() {
        record_check(&exe);
        anyhow::bail!("Asset download returned {}", resp.status());
    }

    let bytes = resp.bytes().await?;
    let binary_data = extract_binary_from_archive(&bytes)?;
    validate_binary(&binary_data)?;

    let staged = suffixed_path(&exe, ".staged");
    std::fs::write(&staged, &binary_data)?;

    // On Unix, make the staged binary executable
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    }

    tracing::info!(
        "Staged update v{} → v{} at {}",
        CURRENT_VERSION,
        remote_version,
        staged.display()
    );
    record_check(&exe);

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Append a suffix to the full path (e.g. `/foo/bar.exe` + `.staged` → `/foo/bar.exe.staged`).
fn suffixed_path(exe: &Path, suffix: &str) -> PathBuf {
    let mut p = exe.as_os_str().to_owned();
    p.push(suffix);
    PathBuf::from(p)
}

/// Returns true if we checked within the last hour (touch-file throttle).
fn should_skip_check(exe: &Path) -> bool {
    let stamp = suffixed_path(exe, ".update-check");
    stamp
        .metadata()
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
        .map(|d| d.as_secs() < 3600)
        .unwrap_or(false)
}

/// Record that we performed an update check (touch the stamp file).
fn record_check(exe: &Path) {
    let stamp = suffixed_path(exe, ".update-check");
    let _ = std::fs::write(&stamp, "");
}

/// Check if auto-update is disabled via env var or config.
fn is_auto_update_disabled() -> bool {
    if std::env::var("CLAUDE_RLM_NO_UPDATE").ok().as_deref() == Some("1") {
        return true;
    }

    // Check [update] section in config TOML
    if let Some(path) = crate::llm::global_config_path() {
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if let Ok(doc) = contents.parse::<toml::Table>() {
                if let Some(update) = doc.get("update").and_then(|v| v.as_table()) {
                    if let Some(enabled) = update.get("auto_update").and_then(|v| v.as_bool()) {
                        return !enabled;
                    }
                }
            }
        }
    }

    false
}

/// Parse a semver string into (major, minor, patch).
fn parse_semver(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim_start_matches('v');
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    Some((
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
    ))
}

/// Returns true if `remote` is strictly newer than `local`.
fn is_newer(remote: &str, local: &str) -> bool {
    match (parse_semver(remote), parse_semver(local)) {
        (Some(r), Some(l)) => r > l,
        _ => false,
    }
}

/// Return the Rust target triple for the current platform.
fn platform_target() -> &'static str {
    if cfg!(target_os = "windows") && cfg!(target_arch = "x86_64") {
        "x86_64-pc-windows-msvc"
    } else if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(target_os = "macos") && cfg!(target_arch = "x86_64") {
        "x86_64-apple-darwin"
    } else if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        "aarch64-apple-darwin"
    } else {
        "unknown"
    }
}

/// Validate that the binary data looks like a real executable.
fn validate_binary(data: &[u8]) -> anyhow::Result<()> {
    if data.len() < 4 {
        anyhow::bail!("Binary too small ({} bytes)", data.len());
    }

    if cfg!(windows) {
        if &data[..2] != b"MZ" {
            anyhow::bail!("Invalid Windows binary (missing MZ header)");
        }
    } else {
        let is_elf = &data[..4] == b"\x7fELF";
        let is_macho = data[..4] == [0xfe, 0xed, 0xfa, 0xce]
            || data[..4] == [0xfe, 0xed, 0xfa, 0xcf]
            || data[..4] == [0xce, 0xfa, 0xed, 0xfe]
            || data[..4] == [0xcf, 0xfa, 0xed, 0xfe];
        if !is_elf && !is_macho {
            anyhow::bail!("Invalid binary (not ELF or Mach-O)");
        }
    }

    Ok(())
}

/// Extract the claude-rlm binary from a zip archive (Windows).
#[cfg(windows)]
fn extract_binary_from_archive(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    use std::io::Read;

    let cursor = std::io::Cursor::new(data);
    let mut archive = zip::ZipArchive::new(cursor)?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name().to_string();
        if name == "claude-rlm.exe" || name.ends_with("/claude-rlm.exe") {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;
            return Ok(buf);
        }
    }

    anyhow::bail!("claude-rlm.exe not found in zip archive")
}

/// Extract the claude-rlm binary from a tar.gz archive (Unix).
#[cfg(not(windows))]
fn extract_binary_from_archive(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    use std::io::Read;

    let gz = flate2::read::GzDecoder::new(data);
    let mut archive = tar::Archive::new(gz);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if name == "claude-rlm" {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            return Ok(buf);
        }
    }

    anyhow::bail!("claude-rlm not found in tar.gz archive")
}

// ---------------------------------------------------------------------------
// GitHub API types
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubAsset>,
}

#[derive(serde::Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_semver() {
        assert_eq!(parse_semver("0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_semver("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_semver("1.0"), None);
        assert_eq!(parse_semver("abc"), None);
    }

    #[test]
    fn test_is_newer() {
        assert!(is_newer("0.3.0", "0.2.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("v0.2.0", "0.2.0"));
        assert!(is_newer("v0.3.0", "0.2.0"));
    }

    #[test]
    fn test_platform_target() {
        let target = platform_target();
        assert_ne!(target, "unknown");
    }
}
