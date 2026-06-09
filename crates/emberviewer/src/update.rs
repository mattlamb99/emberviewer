//! Native-only "check for updates": a single HTTPS GET to the GitHub Releases
//! API, run on a background thread so the UI never blocks. The latest release
//! tag is compared with the running version and reported back over a channel
//! that the UI drains each frame (like the hub / discovery pattern).

use std::sync::mpsc;
use std::time::Duration;

/// Latest-release API endpoint (returns the newest non-prerelease, non-draft).
const RELEASES_API: &str = "https://api.github.com/repos/mattlamb99/emberviewer/releases/latest";
/// Human-facing releases page, used as a fallback link.
const RELEASES_PAGE: &str = "https://github.com/mattlamb99/emberviewer/releases/latest";

/// Outcome of an update check, polled by the UI each frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum UpdateStatus {
    /// No check has run yet (or update checks are disabled).
    #[default]
    Idle,
    /// A check is in flight on the background thread.
    Checking,
    /// The running version is the newest release.
    UpToDate,
    /// A newer release exists.
    Available { version: String, url: String },
    /// The check could not complete (offline, rate-limited, parse error, ...).
    Failed(String),
}

#[derive(serde::Deserialize)]
struct LatestRelease {
    tag_name: String,
    #[serde(default)]
    html_url: String,
}

/// Spawn a one-shot update check on a background thread. The returned receiver
/// yields exactly one [`UpdateStatus`] (`Available` / `UpToDate` / `Failed`).
pub fn spawn_check(current_version: String) -> mpsc::Receiver<UpdateStatus> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(fetch(&current_version));
    });
    rx
}

/// Perform the blocking GET + parse + compare. Every failure path maps to
/// `Failed` so a check never panics the thread.
fn fetch(current: &str) -> UpdateStatus {
    let agent = ureq::builder().timeout(Duration::from_secs(10)).build();
    let resp = match agent
        .get(RELEASES_API)
        // GitHub requires a User-Agent; identify ourselves and the version.
        .set("User-Agent", &format!("emberviewer/{current}"))
        .set("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(r) => r,
        Err(e) => return UpdateStatus::Failed(e.to_string()),
    };
    let body = match resp.into_string() {
        Ok(b) => b,
        Err(e) => return UpdateStatus::Failed(e.to_string()),
    };
    let release: LatestRelease = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => return UpdateStatus::Failed(e.to_string()),
    };
    if is_newer(current, &release.tag_name) {
        let url = if release.html_url.is_empty() {
            RELEASES_PAGE.to_string()
        } else {
            release.html_url
        };
        UpdateStatus::Available {
            version: release.tag_name,
            url,
        }
    } else {
        UpdateStatus::UpToDate
    }
}

/// Is `latest` a newer version than `current`? Both are `X.Y.Z`; `latest` may
/// carry a leading `v`. Any parse failure returns `false`, so a non-numeric or
/// otherwise odd tag never produces a spurious "update available".
pub fn is_newer(current: &str, latest: &str) -> bool {
    match (parse_semver(current), parse_semver(latest)) {
        (Some(cur), Some(new)) => new > cur,
        _ => false,
    }
}

fn parse_semver(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim().trim_start_matches('v');
    let mut it = s.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    let patch = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None; // more than three components: treat as unparseable
    }
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_detection() {
        assert!(is_newer("0.2.4", "v0.2.5"));
        assert!(is_newer("0.2.4", "0.2.5")); // missing the v prefix still works
        assert!(is_newer("0.2.9", "0.3.0"));
        assert!(is_newer("0.2.9", "1.0.0"));
        assert!(is_newer("1.9.9", "v2.0.0"));

        assert!(!is_newer("0.2.4", "v0.2.4")); // equal
        assert!(!is_newer("0.2.5", "v0.2.4")); // older
        assert!(!is_newer("0.3.0", "v0.2.9"));

        // Unparseable / odd tags never count as an update.
        assert!(!is_newer("0.2.4", "nightly"));
        assert!(!is_newer("0.2.4", "v0.2"));
        assert!(!is_newer("0.2.4", "v0.2.4.1"));
        assert!(!is_newer("garbage", "v0.2.5"));
    }

    /// Hits the live GitHub API; run manually with `cargo test -- --ignored`.
    #[test]
    #[ignore = "network: queries the live GitHub Releases API"]
    fn live_check_runs() {
        let rx = spawn_check(env!("CARGO_PKG_VERSION").to_string());
        let status = rx
            .recv_timeout(Duration::from_secs(15))
            .expect("a result within 15s");
        // The running version is a published release, so the check should resolve
        // cleanly to up-to-date (or Available if a newer one shipped) - never Failed.
        assert!(
            matches!(
                status,
                UpdateStatus::UpToDate | UpdateStatus::Available { .. }
            ),
            "unexpected status: {status:?}"
        );
    }
}
