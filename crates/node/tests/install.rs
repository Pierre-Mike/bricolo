//! The `bricolo` command line against a fake GitHub API (`BRICOLO_GITHUB_API`):
//! `bricolo install github:…`, `bricolo update`, `bricolo install --from-lock`, `bricolo pack`
//! and `bricolo wit`. No test reaches the network or the real home folder.

#[path = "../../runtime/tests/support/fake_github.rs"]
mod fake_github;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use bricolo_runtime::lock::Lock;
use fake_github::{FakeGitHub, hello_bundle, scripts_bundle, workspace};

const HELLO: &str = "[\"log\", \"kv\", \"timer\"]";

struct Home {
    _dir: tempfile::TempDir,
    root: PathBuf,
    api: String,
}

impl Home {
    fn new(api: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        Self {
            _dir: dir,
            root,
            api: api.to_string(),
        }
    }

    fn config(&self) -> PathBuf {
        self.root.join("config")
    }

    fn bricolo(&self, args: &[&str]) -> (bool, String) {
        let out: Output = Command::new(env!("CARGO_BIN_EXE_bricolo"))
            .args(args)
            .env("BRICOLO_CONFIG_DIR", self.config())
            .env("BRICOLO_DATA_DIR", self.root.join("data"))
            .env("BRICOLO_GITHUB_API", &self.api)
            .env_remove("GITHUB_TOKEN")
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), text)
    }

    fn lock(&self) -> Lock {
        Lock::load(&self.config().join("plugins.lock")).unwrap()
    }
}

fn publish_v1(gh: &FakeGitHub) {
    gh.release(
        "me/hello",
        "v1.0.0",
        &[
            ("hello.rhplugin.tar.gz", hello_bundle("1.0.0", HELLO)),
            ("tools.rhscripts.tar.gz", scripts_bundle("shout", "one")),
        ],
        true,
        None,
    );
}

#[test]
fn install_update_and_restore_from_github() {
    let gh = FakeGitHub::start();
    publish_v1(&gh);
    let home = Home::new(&gh.url);

    let (ok, out) = home.bricolo(&["install", "github:me/hello", "-y"]);
    assert!(ok, "{out}");
    assert!(out.contains("release github:me/hello@v1.0.0"), "{out}");
    assert!(out.contains("warning: scripts are trusted"), "{out}");
    assert!(home.config().join("scripts/shout.lua").is_file());
    assert_eq!(
        home.lock().plugins["hello"].source,
        "github:me/hello@v1.0.0"
    );

    // Without -y and with no terminal, the scripts and the new capability
    // are refused.
    let other = Home::new(&gh.url);
    let (ok, out) = other.bricolo(&["install", "github:me/hello@v1.0.0"]);
    assert!(!ok, "{out}");
    assert!(out.contains("cancelled"), "{out}");

    let (ok, out) = home.bricolo(&["update"]);
    assert!(ok, "{out}");
    assert!(out.contains("hello is up to date"), "{out}");

    gh.release(
        "me/hello",
        "v1.1.0",
        &[(
            "hello.rhplugin.tar.gz",
            hello_bundle("1.1.0", "[\"log\", \"kv\", \"timer\", \"notify\"]"),
        )],
        true,
        None,
    );
    let (ok, out) = home.bricolo(&["update", "hello"]);
    assert!(!ok, "{out}");
    assert!(out.contains("  + notify"), "{out}");
    assert!(out.contains("stays off until you approve notify"), "{out}");
    assert_eq!(home.lock().plugins["hello"].version, "1.1.0");
    let (ok, out) = home.bricolo(&["update", "hello"]);
    assert!(ok, "{out}");
    assert!(out.contains("waits for approval of notify"), "{out}");
    let (ok, out) = home.bricolo(&["update", "hello", "-y"]);
    assert!(ok, "{out}");
    assert!(out.contains("approved notify"), "{out}");
    let permissions = std::fs::read_to_string(home.config().join("permissions.toml")).unwrap();
    assert!(permissions.contains("notify"), "{permissions}");

    // A new device with only the lock gets the same files.
    let fresh = Home::new(&gh.url);
    std::fs::create_dir_all(fresh.config()).unwrap();
    std::fs::copy(
        home.config().join("plugins.lock"),
        fresh.config().join("plugins.lock"),
    )
    .unwrap();
    let (ok, out) = fresh.bricolo(&["install", "--from-lock", "-y"]);
    assert!(ok, "{out}");
    assert_eq!(fresh.lock(), home.lock());
    assert!(fresh.config().join("scripts/lib/text.lua").is_file());
    let (ok, out) = fresh.bricolo(&["install", "--from-lock"]);
    assert!(ok, "{out}");
    assert!(out.contains("hello: already installed"), "{out}");

    let (ok, out) = home.bricolo(&["uninstall", "tools"]);
    assert!(ok, "{out}");
    assert!(!home.config().join("scripts/shout.lua").exists());
}

#[test]
fn pack_then_install_the_bundle() {
    let home = Home::new("http://127.0.0.1:9");
    fake_github::component("hello");
    let hello = workspace().join("plugins/hello");
    let (ok, out) = home.bricolo(&["pack", hello.to_str().unwrap(), "--out", "dist"]);
    assert!(ok, "{out}");
    let bundle = home.root.join("dist/hello.rhplugin.tar.gz");
    assert!(bundle.is_file(), "{out}");
    assert!(out.contains("gh release create v0.1.0"), "{out}");
    let (ok, out) = home.bricolo(&["install", bundle.to_str().unwrap(), "-y"]);
    assert!(ok, "{out}");
    let entry = &home.lock().plugins["hello"];
    assert!(entry.source.ends_with("hello.rhplugin.tar.gz"), "{entry:?}");

    // A folder of scripts packs too, and a single .lua installs.
    let scripts = home.root.join("my-scripts");
    std::fs::create_dir_all(&scripts).unwrap();
    std::fs::write(scripts.join("a.lua"), "bricolo.log.info('a')").unwrap();
    let (ok, out) = home.bricolo(&["pack", scripts.to_str().unwrap()]);
    assert!(ok, "{out}");
    assert!(home.root.join("my-scripts.rhscripts.tar.gz").is_file());
    let (ok, out) = home.bricolo(&["install", scripts.join("a.lua").to_str().unwrap(), "-y"]);
    assert!(ok, "{out}");
    assert!(home.config().join("scripts/a.lua").is_file());
    let (ok, out) = home.bricolo(&["check", scripts.join("a.lua").to_str().unwrap()]);
    assert!(ok && out.contains("compiles"), "{out}");
    std::fs::write(scripts.join("bad.lua"), "if then").unwrap();
    let (ok, out) = home.bricolo(&["check", scripts.join("bad.lua").to_str().unwrap()]);
    assert!(!ok && out.contains("bad.lua:1"), "{out}");
}

#[test]
fn wit_prints_and_writes_the_package() {
    let home = Home::new("http://127.0.0.1:9");
    let (ok, out) = home.bricolo(&["wit"]);
    assert!(ok && out.contains("clipboard"), "{out}");
    let (ok, out) = home.bricolo(&["wit", "notify"]);
    assert!(ok && out.contains("interface notify"), "{out}");
    let (ok, out) = home.bricolo(&["wit", "all"]);
    assert!(
        ok && out.starts_with("package bricolo:stdlib@0.1.0;"),
        "{out}"
    );
    assert!(out.contains("world plugin"));
    let (ok, _) = home.bricolo(&["wit", "nope"]);
    assert!(!ok);
    let (ok, out) = home.bricolo(&["wit", "all", "--out", "w"]);
    assert!(ok, "{out}");
    let written = std::fs::read_dir(home.root.join("w")).unwrap().count();
    let source = std::fs::read_dir(workspace().join("wit")).unwrap().count();
    assert_eq!(written, source);
    assert!(Path::new(&home.root.join("w/world.wit")).is_file());
    let (ok, out) = home.bricolo(&["new", "--list"]);
    assert!(ok && out.contains("window-tool"), "{out}");
    let (ok, out) = home.bricolo(&["new-script", "--list"]);
    assert!(ok && out.contains("starter"), "{out}");
}
