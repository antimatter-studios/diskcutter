//! Curated distro catalog. The picker UI ("Browse catalog") in the
//! frontend lists groups → images so users don't have to type ISO
//! URLs by hand. The catalog itself is a JSON file — we ship a
//! baseline copy in the binary (so first launch + offline both
//! work) and refresh it on demand from a configurable URL (so the
//! curated list can be updated without an app release).
//!
//! ## Sources, in priority order
//! 1. **Cache** at `app_data_dir()/catalog.json` if present and
//!    fresh (younger than `catalog.refresh_hours`, default 24h).
//! 2. **Bundle** — `include_str!("../catalog.json")` baked at
//!    compile time. Always available; works offline.
//! 3. **Remote** — fetched on `catalog_refresh()` from the URL in
//!    the `catalog.url` config key (default
//!    `https://diskcutter.app/catalog.json`). On success the result
//!    is validated, written to the cache, and emitted as a
//!    `disk-cutter://catalog-updated` event.
//!
//! ## Schema
//! See `src-tauri/catalog.schema.json`. Top level:
//! `{ schema_version: 1, groups: [{ id, name, images: [{...}] }] }`.
//! `schema_version` is checked on parse — a future v2 catalog will
//! be ignored by a v1-only app, with the bundled catalog used
//! instead.
//!
//! Wire format is JSON (not YAML) deliberately: zero new deps
//! (serde_json is already in the tree), strict parser, JSON Schema
//! tooling support, browser-friendly for any future website that
//! wants to render the same file.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

const SUPPORTED_SCHEMA: u32 = 1;
const DEFAULT_CATALOG_URL: &str = "https://diskcutter.app/catalog.json";
const DEFAULT_REFRESH_HOURS: u64 = 24;
const FETCH_TIMEOUT_SECS: u64 = 30;

const BUNDLED_CATALOG: &str = include_str!("../catalog.json");

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Catalog {
    pub schema_version: u32,
    #[serde(default)]
    pub generated_at: Option<String>,
    #[serde(default)]
    pub source_commit: Option<String>,
    pub groups: Vec<CatalogGroup>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CatalogGroup {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub images: Vec<CatalogEntry>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub download_url: String,
    #[serde(default)]
    pub sha256sums_url: String,
    pub homepage: String,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub published_at: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
}

/// Wire shape returned to the frontend. Includes provenance so the
/// UI can show "Last refreshed 4h ago — bundled" / "from
/// diskcutter.app".
#[derive(Serialize, Clone, Debug)]
pub struct CatalogResponse {
    pub catalog: Catalog,
    pub source: CatalogSource,
    /// Wall-clock millis since epoch when this catalog was loaded
    /// (cache mtime for `cached`, build time of the binary for
    /// `bundled`, fetch time for `remote`). 0 if unknown.
    pub loaded_at_ms: u64,
    /// `catalog.url` resolved at call time. Surfaced so the prefs
    /// UI can show "fetching from X".
    pub url: String,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSource {
    /// Loaded from the on-disk cache because it's still fresh.
    Cached,
    /// Cache missing or stale; falling back to the in-binary copy.
    Bundled,
    /// Just downloaded from the remote URL (only via
    /// `catalog_refresh`).
    Remote,
}

/// Parse + validate a JSON catalog string. Public so the publish
/// pipeline / tests can sanity-check a candidate file without going
/// through the full filesystem dance.
pub fn parse(json: &str) -> Result<Catalog, CatalogError> {
    let cat: Catalog =
        serde_json::from_str(json).map_err(|e| CatalogError::Parse(e.to_string()))?;
    if cat.schema_version != SUPPORTED_SCHEMA {
        return Err(CatalogError::SchemaVersion {
            got: cat.schema_version,
            want: SUPPORTED_SCHEMA,
        });
    }
    if cat.groups.is_empty() {
        return Err(CatalogError::Validate("catalog has no groups".into()));
    }
    let mut seen_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for group in &cat.groups {
        if group.id.is_empty() {
            return Err(CatalogError::Validate("group with empty id".into()));
        }
        if group.images.is_empty() {
            return Err(CatalogError::Validate(format!(
                "group {:?} has no images",
                group.id
            )));
        }
        for img in &group.images {
            if !seen_ids.insert(&img.id) {
                return Err(CatalogError::Validate(format!(
                    "duplicate image id {:?}",
                    img.id
                )));
            }
            if !(img.download_url.starts_with("http://")
                || img.download_url.starts_with("https://"))
            {
                return Err(CatalogError::Validate(format!(
                    "image {:?} download_url must be http(s)",
                    img.id
                )));
            }
        }
    }
    Ok(cat)
}

#[derive(Debug)]
pub enum CatalogError {
    Parse(String),
    SchemaVersion { got: u32, want: u32 },
    Validate(String),
    Fetch(String),
    Io(String),
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CatalogError::Parse(m) => write!(f, "parse: {m}"),
            CatalogError::SchemaVersion { got, want } => {
                write!(f, "schema version mismatch: got {got}, want {want}")
            }
            CatalogError::Validate(m) => write!(f, "validate: {m}"),
            CatalogError::Fetch(m) => write!(f, "fetch: {m}"),
            CatalogError::Io(m) => write!(f, "io: {m}"),
        }
    }
}

/// Bundled fallback. Unwrap-safe because the build bakes a known-
/// good file in via `include_str!`; if a future edit breaks the
/// JSON, we'd rather panic at startup than ship a broken binary.
pub fn bundled() -> Catalog {
    parse(BUNDLED_CATALOG).expect("bundled catalog must be valid")
}

fn cache_path(app: &AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_data_dir().ok()?;
    let _ = fs::create_dir_all(&dir);
    Some(dir.join("catalog.json"))
}

fn read_pref(app: &AppHandle, key: &str) -> Option<String> {
    let db = app.try_state::<crate::db::Db>()?;
    let conn = db.0.lock().ok()?;
    conn.query_row(
        "SELECT value FROM config WHERE key = ?1",
        rusqlite::params![key],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .filter(|s| !s.is_empty())
}

fn resolve_url(app: &AppHandle) -> String {
    read_pref(app, "catalog.url").unwrap_or_else(|| DEFAULT_CATALOG_URL.to_string())
}

fn refresh_secs(app: &AppHandle) -> u64 {
    read_pref(app, "catalog.refresh_hours")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_REFRESH_HOURS)
        .saturating_mul(3600)
}

fn cache_age_secs(path: &PathBuf) -> Option<u64> {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    SystemTime::now()
        .duration_since(mtime)
        .ok()
        .map(|d| d.as_secs())
}

/// Pure helper: given a cache age in seconds and a refresh-window
/// in seconds, return whether the cache is fresh enough to serve.
/// Split out so the freshness rule is testable without filesystem
/// round-trips. A `refresh_window_secs` of 0 means "never use the
/// cache, always fall back to bundled until refreshed" — useful as
/// a developer escape hatch.
pub fn is_cache_fresh(cache_age_secs: u64, refresh_window_secs: u64) -> bool {
    refresh_window_secs > 0 && cache_age_secs < refresh_window_secs
}

fn load_cache(app: &AppHandle) -> Option<(Catalog, u64)> {
    let path = cache_path(app)?;
    let age = cache_age_secs(&path)?;
    if !is_cache_fresh(age, refresh_secs(app)) {
        return None;
    }
    let raw = fs::read_to_string(&path).ok()?;
    let cat = parse(&raw).ok()?;
    let loaded_at_ms = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    Some((cat, loaded_at_ms))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[tauri::command]
pub fn catalog_list(app: AppHandle) -> CatalogResponse {
    let url = resolve_url(&app);
    if let Some((cat, loaded_at_ms)) = load_cache(&app) {
        return CatalogResponse {
            catalog: cat,
            source: CatalogSource::Cached,
            loaded_at_ms,
            url,
        };
    }
    CatalogResponse {
        catalog: bundled(),
        source: CatalogSource::Bundled,
        loaded_at_ms: 0,
        url,
    }
}

/// Fetch from `catalog.url`, validate, atomically replace the
/// cache, emit `disk-cutter://catalog-updated`. Errors are
/// returned as strings so the frontend can surface them as a
/// toast without bespoke error handling.
#[tauri::command]
pub fn catalog_refresh(app: AppHandle) -> Result<CatalogResponse, String> {
    let url = resolve_url(&app);
    let body = fetch(&url).map_err(|e| e.to_string())?;
    let cat = parse(&body).map_err(|e| e.to_string())?;
    if let Some(path) = cache_path(&app) {
        // Atomic replace via write-to-temp + rename. Avoids leaving
        // a half-written file if the process dies mid-write.
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, &body).map_err(|e| format!("write cache tmp: {e}"))?;
        fs::rename(&tmp, &path).map_err(|e| format!("rename cache: {e}"))?;
    }
    let response = CatalogResponse {
        catalog: cat,
        source: CatalogSource::Remote,
        loaded_at_ms: now_ms(),
        url,
    };
    let _ = app.emit("disk-cutter://catalog-updated", &response);
    Ok(response)
}

fn fetch(url: &str) -> Result<String, CatalogError> {
    let resp = ureq::get(url)
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
        .call()
        .map_err(|e| CatalogError::Fetch(e.to_string()))?;
    resp.into_string()
        .map_err(|e| CatalogError::Io(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_catalog_parses_and_validates() {
        // Compile-time guarantee: shipping a broken bundled catalog
        // would fail this test. CI catches it before release.
        let cat = bundled();
        assert_eq!(cat.schema_version, SUPPORTED_SCHEMA);
        assert!(!cat.groups.is_empty());
    }

    #[test]
    fn parse_rejects_wrong_schema_version() {
        let bad = r#"{"schema_version": 999, "groups": []}"#;
        match parse(bad) {
            Err(CatalogError::SchemaVersion { got, want }) => {
                assert_eq!(got, 999);
                assert_eq!(want, SUPPORTED_SCHEMA);
            }
            other => panic!("expected SchemaVersion error, got {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_empty_groups() {
        let bad = r#"{"schema_version": 1, "groups": []}"#;
        assert!(matches!(parse(bad), Err(CatalogError::Validate(_))));
    }

    #[test]
    fn parse_rejects_group_with_no_images() {
        let bad = r#"{
          "schema_version": 1,
          "groups": [{"id": "x", "name": "X", "images": []}]
        }"#;
        match parse(bad) {
            Err(CatalogError::Validate(m)) => assert!(m.contains("no images")),
            other => panic!("expected Validate, got {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_duplicate_image_ids_across_groups() {
        let bad = r#"{
          "schema_version": 1,
          "groups": [
            {"id": "g1", "name": "G1", "images": [
              {"id": "dup", "name": "A", "description": "a", "download_url": "https://example.com/a.iso", "homepage": "https://example.com"}
            ]},
            {"id": "g2", "name": "G2", "images": [
              {"id": "dup", "name": "B", "description": "b", "download_url": "https://example.com/b.iso", "homepage": "https://example.com"}
            ]}
          ]
        }"#;
        match parse(bad) {
            Err(CatalogError::Validate(m)) => assert!(m.contains("duplicate")),
            other => panic!("expected Validate, got {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_non_http_download_url() {
        let bad = r#"{
          "schema_version": 1,
          "groups": [{"id": "g", "name": "G", "images": [
            {"id": "x", "name": "X", "description": "x", "download_url": "file:///etc/passwd", "homepage": "https://example.com"}
          ]}]
        }"#;
        match parse(bad) {
            Err(CatalogError::Validate(m)) => assert!(m.contains("http(s)")),
            other => panic!("expected Validate, got {other:?}"),
        }
    }

    #[test]
    fn parse_accepts_minimal_valid_catalog() {
        let ok = r#"{
          "schema_version": 1,
          "groups": [{"id": "g", "name": "G", "images": [
            {"id": "x", "name": "X", "description": "x", "download_url": "https://example.com/x.iso", "homepage": "https://example.com"}
          ]}]
        }"#;
        let cat = parse(ok).unwrap();
        assert_eq!(cat.groups.len(), 1);
        assert_eq!(cat.groups[0].images.len(), 1);
        assert_eq!(cat.groups[0].images[0].id, "x");
    }

    #[test]
    fn parse_accepts_optional_fields_missing() {
        // size_bytes, published_at, arch, sha256sums_url, generated_at all optional.
        let ok = r#"{
          "schema_version": 1,
          "groups": [{"id": "g", "name": "G", "images": [
            {"id": "x", "name": "X", "description": "x", "download_url": "https://example.com/x.iso", "homepage": "https://example.com"}
          ]}]
        }"#;
        let cat = parse(ok).unwrap();
        let img = &cat.groups[0].images[0];
        assert!(img.size_bytes.is_none());
        assert!(img.published_at.is_none());
        assert!(img.arch.is_none());
        assert_eq!(img.sha256sums_url, "");
    }

    #[test]
    fn parse_returns_parse_error_for_garbage() {
        assert!(matches!(parse("not json"), Err(CatalogError::Parse(_))));
    }

    #[test]
    fn is_cache_fresh_returns_false_for_zero_window() {
        // Window of 0 = always-stale escape hatch.
        assert!(!is_cache_fresh(0, 0));
        assert!(!is_cache_fresh(1, 0));
    }

    #[test]
    fn is_cache_fresh_returns_true_inside_window() {
        // 1 hour old, 24 hour window
        assert!(is_cache_fresh(3600, 24 * 3600));
    }

    #[test]
    fn is_cache_fresh_returns_false_outside_window() {
        // 25 hours old, 24 hour window
        assert!(!is_cache_fresh(25 * 3600, 24 * 3600));
    }

    #[test]
    fn is_cache_fresh_boundary_is_strict_less_than() {
        // Exactly at the window edge counts as stale — refresh.
        assert!(!is_cache_fresh(24 * 3600, 24 * 3600));
    }

    #[test]
    fn bundled_groups_have_expected_categories() {
        // Sanity: anyone bumping the bundled catalog accidentally
        // dropping the embedded category will fail this. Catches
        // copy-paste mistakes during version bumps.
        let cat = bundled();
        let group_ids: Vec<&str> = cat.groups.iter().map(|g| g.id.as_str()).collect();
        for required in &["linux-desktop", "linux-server", "embedded", "rescue"] {
            assert!(
                group_ids.contains(required),
                "bundled catalog missing required group {required:?}"
            );
        }
    }

    #[test]
    fn bundled_image_ids_are_globally_unique() {
        let cat = bundled();
        let mut seen = std::collections::HashSet::new();
        for g in &cat.groups {
            for img in &g.images {
                assert!(seen.insert(&img.id), "duplicate id in bundled: {}", img.id);
            }
        }
    }

    #[test]
    fn bundled_catalog_uses_only_https() {
        let cat = bundled();
        for g in &cat.groups {
            for img in &g.images {
                assert!(
                    img.download_url.starts_with("https://"),
                    "{}: download_url must be https",
                    img.id
                );
                assert!(
                    img.homepage.starts_with("https://"),
                    "{}: homepage must be https",
                    img.id
                );
            }
        }
    }

    #[test]
    fn catalog_response_serializes_with_snake_case_source() {
        let r = CatalogResponse {
            catalog: bundled(),
            source: CatalogSource::Bundled,
            loaded_at_ms: 0,
            url: DEFAULT_CATALOG_URL.into(),
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"source\":\"bundled\""), "got {json}",);
    }
}
