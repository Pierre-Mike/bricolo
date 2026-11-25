//! `github:` installs, `bricolo update` and `--from-lock` against a fake GitHub
//! API on a local port. No test reaches the network.

#[path = "support/fake_github.rs"]
mod fake_github;

use std::sync::Arc;

use bricolo_runtime::engine::WasmEngine;
use bricolo_runtime::github::GitHub;
use bricolo_runtime::install::{self, Package};
use bricolo_runtime::lock::Lock;
use bricolo_runtime::source::Source;
use bricolo_runtime::{Options, Paths, Runtime, State, update};
use fake_github::{FakeGitHub, hello_bundle, hello_bundle_from, scripts_bundle};
use tempfile::TempDir;

const HELLO: &str = "[\"log\", \"kv\", \"timer\"]";
const HELLO_CLIP: &str = "[\"log\", \"kv\", \"timer\", \"clipboard\"]";

fn home() -> (TempDir, Paths) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let paths = Paths::new(root.join("config"), root.join("data"));
    (dir, paths)
}

fn source(text: &str) -> Source {
    Source::parse(text).unwrap()
}

/// Installs every package, approving everything.
fn apply_all(paths: &Paths, packages: &[Package]) {
    for package in packages {
        match package {
            Package::Plugin(plan) => {
                install::apply(paths, plan).unwrap();
            }
            Package::Scripts(plan) => {
                install::apply_scripts(paths, plan).unwrap();
            }
        }
    }
}

async fn state_of(paths: &Paths, name: &str) -> (State, Option<String>) {
    let rt: Arc<Runtime> = Runtime::start(paths.clone(), Options::default())
        .await
        .unwrap();
    let info = rt.plugin_status(name).await.unwrap();
    rt.shutdown().await;
    (info.state, info.error)
}

#[test]
fn installs_the_latest_release_and_pins_its_tag() {
    let gh = FakeGitHub::start();
    gh.release(
        "me/hello",
        "v1.0.0",
        &[
            ("hello.rhplugin.tar.gz", hello_bundle("1.0.0", HELLO)),
            ("tools.rhscripts.tar.gz", scripts_bundle("shout", "one")),
            ("README.md", b"ignored".to_vec()),
        ],
        true,
        None,
    );
    let (_dir, paths) = home();
    let engine = WasmEngine::new().unwrap();
    let client = GitHub::new(&gh.url, Some("secret".into()));
    let resolved = install::resolve(&engine, &client, &source("github:me/hello"), None).unwrap();
    assert_eq!(resolved.source.to_string(), "github:me/hello@v1.0.0");
    let names: Vec<&str> = resolved.packages.iter().map(Package::name).collect();
    assert_eq!(names, ["hello", "tools"]);
    apply_all(&paths, &resolved.packages);

    let lock = Lock::load(&paths.lock_file()).unwrap();
    let hello = &lock.plugins["hello"];
    assert_eq!(hello.source, "github:me/hello@v1.0.0");
    assert_eq!(hello.version, "1.0.0");
    assert_eq!(hello.sha256, resolved.packages[0].sha256());
    let tools = &lock.scripts["tools"];
    assert_eq!(tools.version, "v1.0.0");
    assert_eq!(tools.files, ["lib/text.lua", "shout.lua"]);
    assert!(paths.scripts_dir().join("shout.lua").is_file());
    assert!(paths.scripts_dir().join("lib/text.lua").is_file());
    assert!(paths.plugin_dir("hello").join("plugin.wasm").is_file());

    // Every request carried the token: the API and the assets share a host.
    let seen = gh.seen();
    assert!(
        seen.iter()
            .any(|s| s.path == "/repos/me/hello/releases/latest")
    );
    assert!(
        seen.iter()
            .all(|s| s.authorization.as_deref() == Some("Bearer secret")),
        "{seen:?}"
    );
}

#[test]
fn a_tag_picks_that_release_and_the_token_stays_with_the_api() {
    let gh = FakeGitHub::start();
    let storage = FakeGitHub::start();
    gh.release(
        "me/hello",
        "v0.9.0",
        &[("hello.rhplugin.tar.gz", hello_bundle("0.9.0", HELLO))],
        false,
        Some(&storage),
    );
    gh.release(
        "me/hello",
        "v1.0.0",
        &[("hello.rhplugin.tar.gz", hello_bundle("1.0.0", HELLO))],
        true,
        None,
    );
    let engine = WasmEngine::new().unwrap();
    let client = GitHub::new(&gh.url, Some("secret".into()));
    let resolved =
        install::resolve(&engine, &client, &source("github:me/hello@v0.9.0"), None).unwrap();
    let Package::Plugin(plan) = &resolved.packages[0] else {
        panic!("expected a plugin");
    };
    assert_eq!(plan.version, "0.9.0");
    assert_eq!(plan.source, "github:me/hello@v0.9.0");
    // The asset lives on another host, which never sees the token.
    let downloads = storage.seen();
    assert_eq!(downloads.len(), 1);
    assert_eq!(downloads[0].authorization, None);
}

#[test]
fn plain_plugin_assets_work_and_missing_ones_are_explained() {
    let gh = FakeGitHub::start();
    let manifest =
        "name = \"hello\"\nversion = \"2.0.0\"\nimports = [\"log\", \"kv\", \"timer\"]\n";
    let wasm = std::fs::read(fake_github::component("hello")).unwrap();
    gh.release(
        "me/plain",
        "v2",
        &[
            ("plugin.toml", manifest.as_bytes().to_vec()),
            ("plugin.wasm", wasm),
        ],
        true,
        None,
    );
    gh.release(
        "me/empty",
        "v1",
        &[("notes.txt", b"x".to_vec())],
        true,
        None,
    );
    let engine = WasmEngine::new().unwrap();
    let client = GitHub::new(&gh.url, None);
    let resolved = install::resolve(&engine, &client, &source("github:me/plain"), None).unwrap();
    assert_eq!(resolved.packages[0].name(), "hello");

    let err = install::resolve(&engine, &client, &source("github:me/empty"), None).unwrap_err();
    assert!(err.to_string().contains("notes.txt"), "{err}");
    let err = install::resolve(&engine, &client, &source("github:me/nothing"), None).unwrap_err();
    assert!(err.to_string().contains("GITHUB_TOKEN"), "{err}");
    gh.route("/repos/me/limited/releases/latest", 403, b"{}".to_vec());
    let err = install::resolve(&engine, &client, &source("github:me/limited"), None).unwrap_err();
    assert!(err.to_string().contains("refused"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn update_asks_before_new_capabilities_and_keeps_the_plugin_off() {
    let gh = FakeGitHub::start();
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
    let (_dir, paths) = home();
    let engine = WasmEngine::new().unwrap();
    let client = GitHub::new(&gh.url, None);
    let resolved = install::resolve(&engine, &client, &source("github:me/hello"), None).unwrap();
    apply_all(&paths, &resolved.packages);

    // Nothing newer yet.
    let checks = update::check(&engine, &client, &paths, None).unwrap();
    assert_eq!(checks.len(), 2);
    for (_, check) in &checks {
        let check = check.as_ref().unwrap();
        assert!(check.update.is_none() && check.ungranted.is_empty());
    }

    // v1.1.0 asks for the clipboard too, and changes the scripts.
    gh.release(
        "me/hello",
        "v1.1.0",
        &[
            ("hello.rhplugin.tar.gz", hello_bundle("1.1.0", HELLO_CLIP)),
            ("tools.rhscripts.tar.gz", scripts_bundle("shout", "two")),
        ],
        true,
        None,
    );
    let mut checks = update::check(&engine, &client, &paths, Some("hello")).unwrap();
    assert_eq!(checks.len(), 1);
    let check = checks.remove(0).1.unwrap();
    assert_eq!(check.current, "github:me/hello@v1.0.0");
    assert_eq!(check.latest, "v1.1.0");
    assert_eq!(check.added, ["clipboard"]);
    assert!(check.removed.is_empty());
    assert_eq!(check.ungranted, ["clipboard"]);
    let Some(Package::Plugin(plan)) = &check.update else {
        panic!("expected a plugin update");
    };
    // Not approved: the files and the lock move on, the grants don't.
    install::apply_ungranted(&paths, plan).unwrap();
    let lock = Lock::load(&paths.lock_file()).unwrap();
    assert_eq!(lock.plugins["hello"].source, "github:me/hello@v1.1.0");
    let (state, error) = state_of(&paths, "hello").await;
    assert_eq!(state, State::Refused, "{error:?}");
    assert!(error.unwrap_or_default().contains("clipboard"));

    // The next check reports the approval still pending; granting it
    // turns the plugin on.
    let check = update::check(&engine, &client, &paths, Some("hello"))
        .unwrap()
        .remove(0)
        .1
        .unwrap();
    assert!(check.update.is_none());
    assert_eq!(check.ungranted, ["clipboard"]);
    let installed = update::installed_manifest(&paths, "hello").unwrap();
    install::grant_all(&paths, &installed).unwrap();
    let (state, error) = state_of(&paths, "hello").await;
    assert_eq!(state, State::Running, "{error:?}");

    // The script bundle updates without a capability question.
    let check = update::check(&engine, &client, &paths, Some("tools"))
        .unwrap()
        .remove(0)
        .1
        .unwrap();
    let Some(Package::Scripts(plan)) = &check.update else {
        panic!("expected a scripts update");
    };
    install::apply_scripts(&paths, plan).unwrap();
    let shout = std::fs::read_to_string(paths.scripts_dir().join("shout.lua")).unwrap();
    assert!(shout.contains("two"));

    assert!(update::check(&engine, &client, &paths, Some("nope")).is_err());
}

#[test]
fn from_lock_reinstalls_at_the_pinned_hashes() {
    let gh = FakeGitHub::start();
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
    let engine = WasmEngine::new().unwrap();
    let client = GitHub::new(&gh.url, None);
    let (_old, laptop) = home();
    let resolved = install::resolve(&engine, &client, &source("github:me/hello"), None).unwrap();
    apply_all(&laptop, &resolved.packages);
    // A newer release must not matter: the lock pins v1.0.0.
    gh.release(
        "me/hello",
        "v2.0.0",
        &[("hello.rhplugin.tar.gz", hello_bundle("2.0.0", HELLO))],
        true,
        None,
    );

    // A new device gets only the lock.
    let (_new, desktop) = home();
    std::fs::create_dir_all(&desktop.config).unwrap();
    std::fs::copy(laptop.lock_file(), desktop.lock_file()).unwrap();
    let restored = update::restore(&engine, &client, &desktop).unwrap();
    let packages: Vec<Package> = restored
        .into_iter()
        .map(|(name, r)| r.unwrap_or_else(|e| panic!("{name}: {e}")).unwrap())
        .collect();
    assert_eq!(packages.len(), 2);
    let Package::Plugin(plan) = &packages[0] else {
        panic!("expected the plugin first");
    };
    assert_eq!(plan.version, "1.0.0");
    apply_all(&desktop, &packages);
    assert_eq!(
        std::fs::read_to_string(laptop.lock_file()).unwrap(),
        std::fs::read_to_string(desktop.lock_file()).unwrap()
    );
    // Everything is in place now.
    let again = update::restore(&engine, &client, &desktop).unwrap();
    assert!(again.iter().all(|(_, r)| matches!(r, Ok(None))));

    // A release whose component changed under the same tag is refused.
    gh.release(
        "me/hello",
        "v1.0.0",
        &[(
            "hello.rhplugin.tar.gz",
            hello_bundle_from(
                "window-tiler",
                "1.0.0",
                "[\"log\", \"hotkey\", \"window\", \"kv\", \"timer\"]",
            ),
        )],
        false,
        None,
    );
    let (_third, other) = home();
    std::fs::create_dir_all(&other.config).unwrap();
    std::fs::copy(laptop.lock_file(), other.lock_file()).unwrap();
    let restored = update::restore(&engine, &client, &other).unwrap();
    let err = restored[0].1.as_ref().unwrap_err().to_string();
    assert!(err.contains("plugins.lock pins"), "{err}");
}

#[test]
fn local_scripts_and_bundles_install_with_their_source() {
    let (dir, paths) = home();
    let engine = WasmEngine::new().unwrap();
    let client = GitHub::new("http://127.0.0.1:9", None);
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("lib")).unwrap();
    std::fs::write(src.join("clip.lua"), "bricolo.log.info('clip')").unwrap();
    std::fs::write(src.join("lib/util.lua"), "return {}").unwrap();

    let one = install::resolve(
        &engine,
        &client,
        &source(&src.join("clip.lua").display().to_string()),
        None,
    )
    .unwrap();
    let Package::Scripts(plan) = &one.packages[0] else {
        panic!("expected scripts");
    };
    assert_eq!(plan.name, "clip");
    assert_eq!(plan.scripts, ["clip"]);
    install::apply_scripts(&paths, plan).unwrap();

    let folder =
        install::resolve(&engine, &client, &source(&src.display().to_string()), None).unwrap();
    let Package::Scripts(plan) = &folder.packages[0] else {
        panic!("expected scripts");
    };
    assert_eq!(plan.name, "src");
    assert_eq!(plan.files.len(), 2);

    // A packed bundle installs the same files.
    let packed = dir.path().join("src.rhscripts.tar.gz");
    std::fs::write(&packed, bricolo_runtime::bundle::pack(&plan.files).unwrap()).unwrap();
    let from_bundle = install::resolve(
        &engine,
        &client,
        &source(&packed.display().to_string()),
        None,
    )
    .unwrap();
    assert_eq!(from_bundle.packages[0].sha256(), plan.sha256);

    let removed = install::uninstall_scripts(&paths, "clip").unwrap().unwrap();
    assert_eq!(removed, ["clip.lua"]);
    assert!(!paths.scripts_dir().join("clip.lua").exists());
    assert!(
        install::uninstall_scripts(&paths, "clip")
            .unwrap()
            .is_none()
    );
}
