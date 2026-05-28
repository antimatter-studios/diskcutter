// Tiny dev-channel HTTP server. Serves files from a directory and dumps
// request lines so we can see the updater hit. No directory listing, no
// MIME magic — Tauri only fetches `/latest.json` and the artifact URL
// recorded inside it.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use tiny_http::{Header, Method, Response, Server};

fn main() {
    let mut args = env::args().skip(1);
    let root = args
        .next()
        .unwrap_or_else(|| "dev-updates".to_string());
    let port: u16 = args
        .next()
        .unwrap_or_else(|| "17780".to_string())
        .parse()
        .expect("port must be a u16");

    let root_path = PathBuf::from(&root);
    if !root_path.exists() {
        fs::create_dir_all(&root_path).unwrap_or_else(|e| panic!("could not create {root}: {e}"));
        eprintln!("created {root} (no updates published yet)");
    }
    let root = root_path
        .canonicalize()
        .unwrap_or_else(|e| panic!("could not resolve {root}: {e}"));

    let addr = format!("0.0.0.0:{port}");
    let server = Server::http(&addr).expect("bind failed");
    eprintln!("serving {} on http://{}", root.display(), addr);

    for req in server.incoming_requests() {
        eprintln!("{} {}", req.method(), req.url());
        if !matches!(req.method(), Method::Get | Method::Head) {
            let _ = req.respond(Response::from_string("method not allowed").with_status_code(405));
            continue;
        }
        let url_path = req.url().split('?').next().unwrap_or("/");
        let rel = url_path.trim_start_matches('/');
        let path = root.join(rel);
        if !is_inside(&root, &path) {
            let _ = req.respond(Response::from_string("forbidden").with_status_code(403));
            continue;
        }
        match fs::read(&path) {
            Ok(bytes) => {
                let ct = content_type_for(&path);
                let resp = Response::from_data(bytes)
                    .with_header(Header::from_bytes(&b"Content-Type"[..], ct.as_bytes()).unwrap());
                let _ = req.respond(resp);
            }
            Err(_) => {
                let _ = req.respond(Response::from_string("not found").with_status_code(404));
            }
        }
    }
}

fn is_inside(root: &Path, p: &Path) -> bool {
    p.canonicalize()
        .map(|abs| abs.starts_with(root))
        .unwrap_or(false)
}

fn content_type_for(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()) {
        Some("json") => "application/json",
        Some("gz") | Some("tar") => "application/octet-stream",
        Some("sig") => "text/plain",
        _ => "application/octet-stream",
    }
}
