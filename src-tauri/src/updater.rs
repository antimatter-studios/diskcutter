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

fn build(
    app: &AppHandle,
    endpoint: Option<String>,
) -> Result<tauri_plugin_updater::Updater, String> {
    let mut b = app.updater_builder();
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
