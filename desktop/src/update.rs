//! "Check for updates and install": finds the newest GitHub release, downloads the
//! file for this platform, checks it against the release's SHA256SUMS and installs it.
//!
//! Windows: the running exe is renamed aside and the new one takes its place.
//! Linux: the .deb is installed with `pkexec apt-get`, but only from the installed copy.
//! Either way the app then restarts.

use std::{
    fmt, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

use crate::{dialog, engine::Command, log};

/// The GitHub project releases come from: `repository` in Cargo.toml.
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
const RELEASES_PAGE: &str = concat!(env!("CARGO_PKG_REPOSITORY"), "/releases/latest");
/// Published next to the release files by the release workflow.
const CHECKSUMS_FILE: &str = "SHA256SUMS";
const DIALOG_TITLE: &str = "DroidLeap update";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_DOWNLOAD: u64 = 64 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(u64, u64, u64);

impl Version {
    /// Reads `1.2.3` or `v1.2.3`.
    pub fn parse(text: &str) -> Option<Version> {
        let mut parts = text
            .trim_start_matches('v')
            .split('.')
            .map(|part| part.parse().ok());
        let version = Version(parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(version)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[derive(Debug, Deserialize)]
struct ApiRelease {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// A release with a file for this platform and the checksums to verify it with.
#[derive(Debug, PartialEq)]
pub struct Release {
    version: Version,
    file: Asset,
    sums: Asset,
}

impl ApiRelease {
    fn into_release(self) -> Result<Release, String> {
        let version = Version::parse(&self.tag_name)
            .ok_or_else(|| format!("cannot read the release version {:?}", self.tag_name))?;
        let wanted = platform::asset_name(&version);
        let find = |name: &str| {
            self.assets
                .iter()
                .find(|asset| asset.name == name)
                .cloned()
                .ok_or_else(|| format!("release {version} has no {name}"))
        };
        Ok(Release {
            version,
            file: find(&wanted)?,
            sums: find(CHECKSUMS_FILE)?,
        })
    }
}

/// The hash `sha256sum` printed for `name`.
fn expected_hash(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.split_once(char::is_whitespace)?;
        // Binary mode puts a `*` in front of the name.
        (file.trim().trim_start_matches('*') == name).then(|| hash.to_ascii_lowercase())
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn agent() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_global(Some(TRANSFER_TIMEOUT))
        .user_agent(concat!("droidleap/", env!("CARGO_PKG_VERSION")));
    #[cfg(windows)]
    let config = config.tls_config(
        ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .build(),
    );
    config.build().into()
}

fn get(agent: &ureq::Agent, url: &str) -> Result<ureq::Body, String> {
    agent
        .get(url)
        .call()
        .map(|response| response.into_body())
        .map_err(|e| format!("cannot fetch {url}: {e}"))
}

/// The GitHub API address of the latest release of the project at `repository`.
fn latest_release_api(repository: &str) -> Result<String, String> {
    let project = repository
        .strip_prefix("https://github.com/")
        .map(|path| path.trim_end_matches('/'))
        .filter(|path| path.split('/').count() == 2)
        .ok_or_else(|| {
            format!("`repository` in Cargo.toml must be https://github.com/owner/name, not {repository:?}")
        })?;
    Ok(format!(
        "https://api.github.com/repos/{project}/releases/latest"
    ))
}

/// The latest release, if it's newer than `current`.
fn find_newer(agent: &ureq::Agent, current: Version) -> Result<Option<Release>, String> {
    let url = latest_release_api(REPOSITORY)?;
    let text = agent
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("cannot fetch {url}: {e}"))?
        .into_body()
        .read_to_string()
        .map_err(|e| format!("cannot read the release information: {e}"))?;
    let api: ApiRelease = serde_json::from_str(&text)
        .map_err(|e| format!("cannot understand the release information: {e}"))?;
    let release = api.into_release()?;
    Ok((release.version > current).then_some(release))
}

/// Downloads the release file to `dest`, which only keeps it if its checksum matches.
fn download(agent: &ureq::Agent, release: &Release, dest: &Path) -> Result<(), String> {
    let sums = get(agent, &release.sums.browser_download_url)?
        .read_to_string()
        .map_err(|e| format!("cannot read {CHECKSUMS_FILE}: {e}"))?;
    let expected = expected_hash(&sums, &release.file.name)
        .ok_or_else(|| format!("{CHECKSUMS_FILE} has no entry for {}", release.file.name))?;

    let fetch = || -> Result<(), String> {
        let mut body = get(agent, &release.file.browser_download_url)?;
        let mut reader = body.with_config().limit(MAX_DOWNLOAD).reader();
        let mut file =
            fs::File::create(dest).map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
        let mut hasher = Sha256::new();
        let mut chunk = [0u8; 64 << 10];
        loop {
            let n = reader
                .read(&mut chunk)
                .map_err(|e| format!("download failed: {e}"))?;
            if n == 0 {
                break;
            }
            hasher.update(&chunk[..n]);
            file.write_all(&chunk[..n])
                .map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
        }
        file.flush()
            .map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
        let actual = hex(&hasher.finalize());
        if actual != expected {
            return Err(format!(
                "the download is corrupt: its SHA-256 is {actual}, not {expected}"
            ));
        }
        Ok(())
    };
    let result = fetch();
    if result.is_err() {
        let _ = fs::remove_file(dest);
    }
    result
}

fn stage_and_install(release: &Release, exe: &Path) -> Result<(), String> {
    let staged = platform::staging_path(exe, &release.file.name)?;
    download(&agent(), release, &staged)?;
    let result = platform::install(exe, &staged);
    // After a Windows install the file has moved, so this finds nothing.
    let _ = fs::remove_file(&staged);
    result
}

/// Clears what an earlier update left behind. Call at startup.
pub fn cleanup(exe: &Path) {
    platform::cleanup(exe);
}

enum Outcome {
    UpToDate(Version),
    Installed(Version),
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| format!("update task failed: {e}"))?
}

async fn check_and_install(exe: &Path) -> Result<Outcome, String> {
    let current = Version::parse(env!("CARGO_PKG_VERSION")).ok_or("cannot read the app version")?;
    log("checking for updates");
    let Some(release) = blocking(move || find_newer(&agent(), current)).await? else {
        return Ok(Outcome::UpToDate(current));
    };
    log(format!(
        "downloading {} ({current} installed)",
        release.file.name
    ));
    let version = release.version;
    let exe = exe.to_path_buf();
    blocking(move || stage_and_install(&release, &exe)).await?;
    Ok(Outcome::Installed(version))
}

/// Handles "check for updates" requests one at a time. After installing, asks the engine to restart.
pub async fn run(
    mut requests: mpsc::UnboundedReceiver<()>,
    commands: mpsc::UnboundedSender<Command>,
    exe: PathBuf,
) {
    while requests.recv().await.is_some() {
        match check_and_install(&exe).await {
            Ok(Outcome::UpToDate(version)) => {
                log(format!("DroidLeap {version} is the latest version"));
                dialog::show(
                    DIALOG_TITLE,
                    &format!("DroidLeap {version} is the latest version."),
                )
                .await;
            }
            Ok(Outcome::Installed(version)) => {
                log(format!("updated to {version}, restarting"));
                let _ = commands.send(Command::Restart); // only fails once the engine has stopped
            }
            Err(e) => {
                log(format!("update failed: {e}"));
                dialog::show(
                    DIALOG_TITLE,
                    &format!("The update failed: {e}\n\nYou can download it from {RELEASES_PAGE}"),
                )
                .await;
            }
        }
        // Clicks made while this ran asked for the same thing.
        while requests.try_recv().is_ok() {}
    }
}

#[cfg(windows)]
mod platform {
    use super::*;

    pub fn asset_name(version: &Version) -> String {
        format!("droidleap-{version}-windows-x64.exe")
    }

    /// Next to the exe, so the final rename can't cross volumes.
    pub fn staging_path(exe: &Path, _asset: &str) -> Result<PathBuf, String> {
        Ok(exe.with_extension("exe.new"))
    }

    /// A running exe can be renamed but not overwritten, so it moves aside and the new one takes its name.
    pub fn install(exe: &Path, staged: &Path) -> Result<(), String> {
        let old = exe.with_extension("exe.old");
        let _ = fs::remove_file(&old);
        fs::rename(exe, &old).map_err(|e| format!("cannot replace {}: {e}", exe.display()))?;
        if let Err(e) = fs::rename(staged, exe) {
            let _ = fs::rename(&old, exe);
            return Err(format!("cannot replace {}: {e}", exe.display()));
        }
        Ok(())
    }

    pub fn cleanup(exe: &Path) {
        let _ = fs::remove_file(exe.with_extension("exe.old"));
    }
}

#[cfg(not(windows))]
mod platform {
    use std::{fs::DirBuilder, os::unix::fs::DirBuilderExt, process::Command as Process};

    use super::*;

    pub fn asset_name(version: &Version) -> String {
        format!("droidleap_{version}_amd64.deb")
    }

    /// In the user's private runtime directory, so nobody else can swap the file before root installs it.
    pub fn staging_path(_exe: &Path, asset: &str) -> Result<PathBuf, String> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR is not set")?;
        let dir = PathBuf::from(runtime).join("droidleap-update");
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        Ok(dir.join(asset))
    }

    pub fn install(exe: &Path, deb: &Path) -> Result<(), String> {
        if !exe.starts_with("/usr/") {
            return Err(format!(
                "{} is not the installed package, so it can't update itself",
                exe.display()
            ));
        }
        let output = Process::new("pkexec")
            .args(["apt-get", "install", "-y"])
            .arg(deb)
            .output()
            .map_err(|e| format!("cannot run pkexec: {e}"))?;
        if output.status.success() {
            return Ok(());
        }
        // pkexec exits with 126 or 127 when the password prompt was dismissed or failed.
        if matches!(output.status.code(), Some(126 | 127)) {
            return Err("the installation was not authorized".into());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(3).collect();
        Err(format!(
            "apt-get failed ({}): {}",
            output.status,
            tail.into_iter().rev().collect::<Vec<_>>().join(" ")
        ))
    }

    pub fn cleanup(_exe: &Path) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_and_compare_numerically() {
        assert_eq!(Version::parse("v0.5.2"), Some(Version(0, 5, 2)));
        assert_eq!(Version::parse("1.10.0"), Some(Version(1, 10, 0)));
        assert!(Version(0, 10, 0) > Version(0, 9, 9));
        assert!(Version(0, 5, 2) > Version(0, 5, 1));
        for bad in ["", "1.2", "1.2.3.4", "1.2.x", "v", "1.2.3-rc1"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert_eq!(Version(0, 5, 2).to_string(), "0.5.2");
    }

    #[test]
    fn finds_the_checksum_for_a_file() {
        let sums = "\
aaaa11  droidleap-0.5.2-windows-x64.exe
BBBB22 *droidleap_0.5.2_amd64.deb
cccc33  droidleap-0.5.2.apk
";
        assert_eq!(
            expected_hash(sums, "droidleap_0.5.2_amd64.deb").as_deref(),
            Some("bbbb22")
        );
        assert_eq!(
            expected_hash(sums, "droidleap-0.5.2.apk").as_deref(),
            Some("cccc33")
        );
        assert_eq!(expected_hash(sums, "missing"), None);
    }

    #[test]
    fn the_api_address_comes_from_the_repository_url() {
        assert_eq!(
            latest_release_api("https://github.com/owner/name").as_deref(),
            Ok("https://api.github.com/repos/owner/name/releases/latest")
        );
        assert!(latest_release_api("https://github.com/owner/name/").is_ok());
        for bad in [
            "",
            "https://gitlab.com/owner/name",
            "https://github.com/owner",
        ] {
            assert!(latest_release_api(bad).is_err(), "{bad}");
        }
        assert!(
            latest_release_api(REPOSITORY).is_ok(),
            "Cargo.toml's repository"
        );
    }

    #[test]
    fn hex_is_lowercase_and_padded() {
        assert_eq!(hex(&[0x00, 0x0a, 0xff]), "000aff");
        assert_eq!(
            hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    fn release_json(tag: &str, files: &[&str]) -> ApiRelease {
        let assets: Vec<String> = files
            .iter()
            .map(|name| {
                format!(
                    r#"{{"name":"{name}","browser_download_url":"https://example.test/{name}"}}"#
                )
            })
            .collect();
        serde_json::from_str(&format!(
            r#"{{"tag_name":"{tag}","assets":[{}]}}"#,
            assets.join(",")
        ))
        .unwrap()
    }

    #[test]
    fn picks_this_platforms_file_and_the_checksums() {
        let wanted = platform::asset_name(&Version(0, 5, 3));
        let release = release_json("v0.5.3", &["droidleap-0.5.3.apk", &wanted, CHECKSUMS_FILE])
            .into_release()
            .unwrap();
        assert_eq!(release.version, Version(0, 5, 3));
        assert_eq!(release.file.name, wanted);
        assert_eq!(release.sums.name, CHECKSUMS_FILE);
    }

    #[test]
    fn a_release_without_the_file_or_checksums_is_unusable() {
        let wanted = platform::asset_name(&Version(0, 5, 3));
        assert!(
            release_json("v0.5.3", &[CHECKSUMS_FILE])
                .into_release()
                .is_err()
        );
        assert!(
            release_json("v0.5.3", &[&wanted]).into_release().is_err(),
            "unverifiable downloads are refused"
        );
        assert!(release_json("nightly", &[]).into_release().is_err());
    }
}
