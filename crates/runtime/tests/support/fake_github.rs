//! A fake GitHub REST API on a local port, for `bricolo install github:…`
//! tests. It serves release JSON and asset bytes from a table that tests
//! fill, and records each request's path and `Authorization` header.
//!
//! Shared by `crates/runtime/tests/github.rs` and the `bricolo` command line
//! tests in `crates/node/tests/install.rs`.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

type Routes = Arc<Mutex<HashMap<String, (u16, Vec<u8>)>>>;

#[derive(Clone, Debug)]
pub struct Seen {
    pub path: String,
    pub authorization: Option<String>,
}

pub struct FakeGitHub {
    /// `http://127.0.0.1:<port>`, the value for `BRICOLO_GITHUB_API`.
    pub url: String,
    routes: Routes,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl FakeGitHub {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let routes: Routes = Arc::default();
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let (table, log) = (routes.clone(), seen.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                let mut authorization = None;
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).is_err() || h.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.trim().split_once(':')
                        && k.eq_ignore_ascii_case("authorization")
                    {
                        authorization = Some(v.trim().to_string());
                    }
                }
                log.lock().unwrap().push(Seen {
                    path: path.clone(),
                    authorization,
                });
                let (status, body) = table
                    .lock()
                    .unwrap()
                    .get(&path)
                    .cloned()
                    .unwrap_or((404, b"{\"message\":\"Not Found\"}".to_vec()));
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
            }
        });
        Self { url, routes, seen }
    }

    pub fn route(&self, path: &str, status: u16, body: Vec<u8>) {
        self.routes
            .lock()
            .unwrap()
            .insert(path.to_string(), (status, body));
    }

    /// Publishes release `tag` of `repo` ("owner/name"). Assets are served
    /// by `assets_from` (this server, or another one standing in for
    /// GitHub's storage host). With `latest`, it also becomes
    /// `/releases/latest`.
    pub fn release(
        &self,
        repo: &str,
        tag: &str,
        assets: &[(&str, Vec<u8>)],
        latest: bool,
        assets_from: Option<&FakeGitHub>,
    ) {
        let host = assets_from.unwrap_or(self);
        let mut list = Vec::new();
        for (i, (name, bytes)) in assets.iter().enumerate() {
            let path = format!("/repos/{repo}/releases/assets/{tag}-{i}");
            host.route(&path, 200, bytes.clone());
            list.push(serde_json::json!({
                "name": name,
                "url": format!("{}{path}", host.url),
                "browser_download_url": format!("https://github.com/{repo}/releases/download/{tag}/{name}"),
            }));
        }
        let json = serde_json::to_vec(&serde_json::json!({
            "tag_name": tag,
            "draft": false,
            "prerelease": false,
            "assets": list,
        }))
        .unwrap();
        self.route(
            &format!("/repos/{repo}/releases/tags/{tag}"),
            200,
            json.clone(),
        );
        if latest {
            self.route(&format!("/repos/{repo}/releases/latest"), 200, json);
        }
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

pub fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The built component, building it first if it is missing.
pub fn component(name: &str) -> PathBuf {
    let root = workspace();
    let file = name.replace('-', "_");
    let path = root.join(format!("target/wasm32-wasip2/release/{file}.wasm"));
    if !path.is_file() {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = Command::new(cargo)
            .args([
                "build",
                "-p",
                name,
                "--target",
                "wasm32-wasip2",
                "--release",
            ])
            .current_dir(&root)
            .status()
            .expect("cannot run cargo");
        assert!(
            status.success(),
            "building {name} failed; run `just plugins`"
        );
    }
    path
}

/// A `hello.rhplugin.tar.gz` holding plugins/hello's component with this
/// version and imports.
pub fn hello_bundle(version: &str, imports: &str) -> Vec<u8> {
    hello_bundle_from("hello", version, imports)
}

/// Like [`hello_bundle`], with another component under the same name.
pub fn hello_bundle_from(component_name: &str, version: &str, imports: &str) -> Vec<u8> {
    let manifest = format!(
        "name = \"hello\"\nversion = \"{version}\"\nimports = {imports}\n\n[settings]\ngreeting = \"hi\"\ntick_ms = 50\n"
    );
    let wasm = std::fs::read(component(component_name)).unwrap();
    let files = std::collections::BTreeMap::from([
        ("plugin.toml".to_string(), manifest.into_bytes()),
        ("plugin.wasm".to_string(), wasm),
    ]);
    bricolo_runtime::bundle::pack(&files).unwrap()
}

/// A scripts bundle with one script that logs `line`, and a module.
pub fn scripts_bundle(script: &str, line: &str) -> Vec<u8> {
    let files = std::collections::BTreeMap::from([
        (
            format!("{script}.lua"),
            format!(
                "local text = require(\"lib.text\")\nbricolo.log.info(text.upper(\"{line}\"))\n"
            )
            .into_bytes(),
        ),
        (
            "lib/text.lua".to_string(),
            b"return { upper = function(s) return s:upper() end }\n".to_vec(),
        ),
    ]);
    bricolo_runtime::bundle::pack(&files).unwrap()
}
