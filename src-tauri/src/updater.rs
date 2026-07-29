use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error as TlsError, SignatureScheme};
use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tauri::Url;
use tauri_plugin_updater::UpdaterExt;

/// Cert verifier that accepts any certificate — used only for localhost dev server.
#[derive(Debug)]
struct AcceptAnyCert(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn localhost_tls_config() -> Result<rustls::ClientConfig, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(AcceptAnyCert(provider.clone()));
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS12, &rustls::version::TLS13])
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok(cfg)
}

fn localhost_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .use_preconfigured_tls(localhost_tls_config()?)
        .build()
        .map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct UpdateInfo {
    pub current_version: String,
    pub version: String,
    pub body: Option<String>,
    pub date: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct UpdateEntry {
    pub version: String,
    pub notes: Option<String>,
    pub pub_date: Option<String>,
    pub platforms: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct UpdatesManifest {
    #[serde(default)]
    dev: Vec<UpdateEntry>,
}

fn is_localhost(url: &str) -> bool {
    Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h == "localhost" || h == "127.0.0.1"))
        .unwrap_or(false)
}

/// Where a version sits in the release scheme: the CalVer date, which channel
/// it belongs to, and its counter within that date.
///
/// Ordering is (date, channel, counter) with stable outranking dev on the same
/// date, which is what you would expect and what plain semver refuses to do.
#[derive(PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Release {
    date: (u64, u64, u64),
    /// 0 = dev, 1 = stable. A stable release supersedes every dev build of the
    /// same date; a dev build never supersedes a stable one.
    channel: u8,
    counter: u64,
}

/// Parse `YYYY.M.D-0` (stable), `YYYY.M.D-dev.N` (dev), or a bare `YYYY.M.D`.
///
/// A trailing branch slug on a dev version (`-dev.3-my-branch`) is ignored: two
/// builds with the same counter from different branches are not meaningfully
/// ordered against each other, and the dev panel installs by explicit choice
/// rather than by comparison.
fn parse_release(v: &semver::Version) -> Release {
    let date = (v.major, v.minor, v.patch);
    let pre = v.pre.as_str();
    if pre.is_empty() {
        // No suffix at all — treat as the first stable of that date.
        return Release {
            date,
            channel: 1,
            counter: 0,
        };
    }
    if let Some(rest) = pre.strip_prefix("dev.") {
        let counter = rest
            .split(['.', '-'])
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        return Release {
            date,
            channel: 0,
            counter,
        };
    }
    // Stable: a bare numeric suffix. Anything unrecognised (an `-rc1`, say)
    // parses to counter 0, so it never outranks a numbered stable release of
    // the same date.
    let counter = pre
        .split(['.', '-'])
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    Release {
        date,
        channel: 1,
        counter,
    }
}

/// Replaces tauri's default `remote > current` semver test.
///
/// Semver ranks numeric prerelease identifiers below alphanumeric ones, so it
/// considers `2026.7.29-1` (a stable hotfix) OLDER than `2026.7.29-dev.1`. With
/// the default comparator, a machine on a dev build that switched to the stable
/// channel was told it was already up to date while running unreleased code.
///
/// This orders by the scheme's actual meaning instead — see scripts/version.sh.
fn is_newer(current: &semver::Version, remote: &semver::Version) -> bool {
    parse_release(remote) > parse_release(current)
}

fn build(
    app: &AppHandle,
    endpoint: Option<String>,
) -> Result<tauri_plugin_updater::Updater, String> {
    let mut b = app
        .updater_builder()
        .version_comparator(|current, release| is_newer(&current, &release.version));
    let local = endpoint.as_deref().map(is_localhost).unwrap_or(false);
    if let Some(url) = endpoint {
        let parsed = Url::parse(&url).map_err(|e| format!("bad endpoint URL: {e}"))?;
        b = b.endpoints(vec![parsed]).map_err(|e| e.to_string())?;
    }
    // For localhost dev server: use a custom TLS verifier that accepts any cert.
    if local {
        if let Ok(tls) = localhost_tls_config() {
            b = b.configure_client(move |c| c.use_preconfigured_tls(tls.clone()));
        }
    }
    #[cfg(debug_assertions)]
    if !local {
        b = b.configure_client(|c| c.danger_accept_invalid_certs(true));
    }
    b.build().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn updater_check(
    app: AppHandle,
    endpoint: Option<String>,
) -> Result<Option<UpdateInfo>, String> {
    let updater = build(&app, endpoint)?;
    let update = updater.check().await.map_err(|e| e.to_string())?;
    Ok(update.map(|u| UpdateInfo {
        current_version: u.current_version.clone(),
        version: u.version.clone(),
        body: u.body.clone(),
        date: u.date.map(|d| d.to_string()),
    }))
}

#[tauri::command]
pub async fn updater_install(app: AppHandle, endpoint: Option<String>) -> Result<(), String> {
    let updater = build(&app, endpoint)?;
    let update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no update available".to_string())?;
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Fetch updates.json and return up to 10 dev-channel entries (newest first).
/// Self-signed certs are accepted — dev server runs locally over HTTPS without a trusted CA.
#[tauri::command]
pub async fn updater_fetch_updates(endpoint: String) -> Result<Vec<UpdateEntry>, String> {
    let base = endpoint
        .rsplit_once('/')
        .map(|(b, _)| b.to_string())
        .unwrap_or(endpoint.clone());
    let url = format!("{base}/updates.json");

    let client = if is_localhost(&endpoint) {
        localhost_client()?
    } else {
        reqwest::Client::new()
    };

    let manifest: UpdatesManifest = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("updates fetch failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("updates parse failed: {e}"))?;

    Ok(manifest.dev.into_iter().take(10).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> semver::Version {
        semver::Version::parse(s).unwrap_or_else(|e| panic!("{s} must be valid semver: {e}"))
    }

    /// Every version this scheme emits has to survive semver parsing, because
    /// tauri stores it as a `semver::Version` before we ever see it.
    #[test]
    fn every_scheme_version_is_valid_semver() {
        for s in [
            "2026.7.29-0",
            "2026.7.29-1",
            "2026.7.29-12",
            "2026.7.29-dev.1",
            "2026.7.29-dev.42",
            "2026.7.29-dev.3-my-branch",
            "2026.7.29",
        ] {
            let _ = v(s);
        }
    }

    #[test]
    fn a_same_day_hotfix_is_an_update() {
        assert!(is_newer(&v("2026.7.29-0"), &v("2026.7.29-1")));
        assert!(is_newer(&v("2026.7.29-1"), &v("2026.7.29-2")));
        assert!(!is_newer(&v("2026.7.29-2"), &v("2026.7.29-1")));
    }

    /// The bug this comparator exists for. Plain semver ranks numeric
    /// prerelease identifiers below alphanumeric ones, so it puts the stable
    /// hotfix BELOW the dev build and reports "up to date".
    #[test]
    fn stable_supersedes_a_dev_build_of_the_same_date() {
        assert!(is_newer(&v("2026.7.29-dev.9"), &v("2026.7.29-1")));
        assert!(!is_newer(&v("2026.7.29-1"), &v("2026.7.29-dev.9")));

        // Confirm raw semver really does disagree — if this ever stops being
        // true, the comparator is no longer earning its keep.
        assert!(
            v("2026.7.29-1") < v("2026.7.29-dev.9"),
            "semver ordering changed; re-evaluate whether the comparator is needed"
        );
    }

    #[test]
    fn a_later_date_always_wins() {
        assert!(is_newer(&v("2026.7.29-9"), &v("2026.7.30-0")));
        assert!(is_newer(&v("2026.7.29-dev.9"), &v("2026.7.30-dev.1")));
        assert!(is_newer(&v("2026.12.31-0"), &v("2027.1.1-0")));
        assert!(!is_newer(&v("2026.7.30-0"), &v("2026.7.29-9")));
    }

    #[test]
    fn dev_builds_advance_among_themselves() {
        assert!(is_newer(&v("2026.7.29-dev.1"), &v("2026.7.29-dev.2")));
        assert!(!is_newer(&v("2026.7.29-dev.2"), &v("2026.7.29-dev.1")));
    }

    #[test]
    fn a_branch_slug_does_not_change_the_ordering() {
        assert!(is_newer(
            &v("2026.7.29-dev.1-some-branch"),
            &v("2026.7.29-dev.2")
        ));
        assert!(is_newer(
            &v("2026.7.29-dev.3-some-branch"),
            &v("2026.7.29-0")
        ));
    }

    #[test]
    fn the_same_version_is_never_an_update() {
        for s in ["2026.7.29-0", "2026.7.29-dev.4", "2026.7.29"] {
            assert!(!is_newer(&v(s), &v(s)), "{s} should not update to itself");
        }
    }

    /// Legacy dev builds predate the `dev.` prefix and read as stable under
    /// this parser. That is deliberate: the alternative is guessing, and
    /// treating an unknown numeric suffix as stable keeps a real stable
    /// release from being withheld.
    #[test]
    fn a_bare_date_counts_as_the_first_stable_of_that_date() {
        assert!(is_newer(&v("2026.7.29-dev.5"), &v("2026.7.29")));
        assert!(!is_newer(&v("2026.7.29"), &v("2026.7.29-0")));
        assert!(is_newer(&v("2026.7.29"), &v("2026.7.29-1")));
    }
}
