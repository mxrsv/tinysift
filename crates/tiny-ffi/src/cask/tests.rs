use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::test_support::*;
use super::*;

const GHOSTTY_ID: &str = "com.mitchellh.ghostty";
const INSTALL: &str = "install --cask ghostty";

fn token() -> Arc<CancellationToken> {
    CancellationToken::new()
}

/// A Mac with Homebrew where `ghostty` can be installed.
fn installable() -> World {
    let world = World::new();
    world.info("ghostty", Spec::default());
    world
}

fn preview_install(world: &World) -> FfiCaskPreview {
    world
        .session
        .install_preview("ghostty", &world.seams())
        .unwrap()
}

fn outdated_spec(app: &str) -> Spec<'_> {
    Spec {
        version: "2.0",
        installed: Some("1.0"),
        outdated: true,
        app,
        ..Spec::default()
    }
}

/// A Mac whose Homebrew lists `ghostty` and `iterm2` as outdated, with the
/// outdated list already read.
fn updatable() -> World {
    let world = World::new();
    world.outdated_list(&[("ghostty", "1.0", "2.0"), ("iterm2", "1.0", "2.0")]);
    world.info_all(&[
        ("ghostty", outdated_spec("Ghostty")),
        ("iterm2", outdated_spec("iTerm")),
    ]);
    world.install_app("Ghostty", GHOSTTY_ID);
    world.install_app("iTerm", "com.googlecode.iterm2");
    world.session.outdated(&world.seams()).unwrap();
    world
}

fn upgrade_replies(world: &World, tokens: &[&str]) {
    for token in tokens {
        world.brew.reply(
            &format!("upgrade --cask {token}"),
            0,
            "==> Upgrading x\n==> Moving App 'x.app'\n",
            "",
        );
    }
}

fn preview_update(world: &World, tokens: &[&str]) -> FfiCaskPreview {
    let tokens: Vec<String> = tokens.iter().map(|t| t.to_string()).collect();
    world
        .session
        .update_preview(&tokens, &world.seams())
        .unwrap()
}

mod catalogue;
mod install;
mod update;
