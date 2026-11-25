//! The Lua script tier. Each top-level `scripts/*.lua` file runs in its own
//! sandboxed Lua 5.4 state and reaches the machine only through the `bricolo`
//! global, which calls the same providers and router as WASM plugins.
//!
//! - Only files directly in `scripts/` run. Files in subfolders, such as
//!   `scripts/lib/util.lua`, are modules: they load when a script calls
//!   `require("lib.util")`, and never on their own.
//! - Calls that wait (`bricolo.speech.transcribe`, `bricolo.timer.sleep`) yield the
//!   script's coroutine. Each script body, `init`, `dispose` and callback
//!   runs in a coroutine of its own, so a script never writes callbacks for
//!   them.
//! - A failing call returns `nil, err`, where `err` is `"unavailable"`,
//!   `"timeout"`, `"denied"` or `"failed: <message>"`.
//! - An uncaught error restarts the script once; a second one turns it off
//!   until its file changes or `bricolo` reloads it. Other scripts keep running.
//!
//! See docs/scripts.md for the script-facing reference.

mod clock;
mod mailbox;
mod sandbox;
mod script;
mod stdlib;
mod tier;

pub use mailbox::Mailbox;
pub use script::{ScriptInfo, State};
pub use tier::{Limits, Options, ScriptStatus, Scripts, caller_name, valid_name};

/// Compiles a script without running it, for `bricolo check <file>.lua`.
/// Reports syntax errors only: a misspelled `bricolo` call shows up when the
/// script runs.
pub fn check_syntax(source: &[u8], name: &str) -> Result<(), String> {
    let lua = mlua::Lua::new();
    lua.load(source)
        .set_name(format!("@{name}"))
        .into_function()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn check_syntax_reports_the_line() {
        assert!(super::check_syntax(b"bricolo.log.info('ok')", "a.lua").is_ok());
        let err = super::check_syntax(b"local x =\nend", "b.lua").unwrap_err();
        assert!(err.contains("b.lua:2"), "{err}");
    }
}
