use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tauri::Url;
use tauri_plugin_updater::UpdaterExt;

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

fn build(
    app: &AppHandle,
    endpoint: Option<String>,
) -> Result<tauri_plugin_updater::Updater, String> {
    let mut b = app.updater_builder();
    if let Some(url) = endpoint {
        let parsed = Url::parse(&url).map_err(|e| format!("bad endpoint URL: {e}"))?;
        b = b.endpoints(vec![parsed]).map_err(|e| e.to_string())?;
    }
    // In debug builds the dev server uses a self-signed cert; skip validation there only.
    #[cfg(debug_assertions)]
    {
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

    let mut client_builder = reqwest::Client::builder();
    #[cfg(debug_assertions)]
    {
        client_builder = client_builder.danger_accept_invalid_certs(true);
    }
    let client = client_builder.build().map_err(|e| e.to_string())?;

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
