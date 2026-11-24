//! Single-source-of-truth application version & semver helpers.
//!
//! Reads `AETHERSHARE_VERSION` emitted by `build.rs` from the repository root
//! `VERSION` file (falling back to `CARGO_PKG_VERSION`), ensuring the CLI,
//! desktop UI, API status, and GitHub Actions release bundles all report the
//! exact same version.

/// Current application version (e.g. `"0.1.0"`).
pub const APP_VERSION: &str = env!("AETHERSHARE_VERSION");

/// Canonical GitHub repository URL.
pub const REPO_URL: &str = "https://github.com/amarenyo/AetherShare";

/// Parses a semantic version string such as `"0.1.0"`, `"v0.2.3"`, or `"1.0.0-beta.1"`.
pub fn parse_semver(raw: &str) -> Option<(u64, u64, u64)> {
    let cleaned = raw.trim().trim_start_matches('v').trim_start_matches('V');
    let core = cleaned.split('-').next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

/// Returns `true` if `candidate` is strictly newer than `current`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_semver(candidate), parse_semver(current)) {
        (Some(c), Some(cur)) => c > cur,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_version_is_valid_semver() {
        assert!(
            parse_semver(APP_VERSION).is_some(),
            "APP_VERSION ({APP_VERSION}) must be valid semver"
        );
    }

    #[test]
    fn semver_comparison_works() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("0.10.0", "0.2.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
    }
}
