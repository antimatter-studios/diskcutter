use serde::Serialize;
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

fn build(
    app: &AppHandle,
    endpoint: Option<String>,
) -> Result<tauri_plugin_updater::Updater, String> {
    let mut b = app.updater_builder();
    if let Some(url) = endpoint {
        let parsed = Url::parse(&url).map_err(|e| format!("bad endpoint URL: {e}"))?;
        b = b.endpoints(vec![parsed]).map_err(|e| e.to_string())?;
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
