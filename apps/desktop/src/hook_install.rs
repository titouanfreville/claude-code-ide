//! `moonlight hooks {install,uninstall,status}` — manage MoonlightCode's Claude
//! Code hook registration in `~/.claude/settings.json`, plus the Antigravity (`agy`)
//! plugin gate this desktop app also supports.
//!
//! The actual install/uninstall/status logic (backup, interactive confirm, write —
//! and what's registered, and how it's recognized) lives in
//! `moonlight_control::hook_registration`, shared with the headless `moonlightd`
//! daemon (which has the same CLI, minus `agy`) so every binary that can manage
//! hooks does it exactly the same way.

/// Dispatch a `hooks` subcommand. `args` is the full process argv:
/// `moonlight hooks <install|uninstall|status> [claude|agy|all]` (target defaults to
/// `claude` for back-compat). `agy` manages the Antigravity plugin gate; `all` does both.
pub fn run(args: &[String]) {
    let sub = args.get(2).map(String::as_str);
    let target = args.get(3).map(String::as_str);
    let do_claude = matches!(target, None | Some("claude") | Some("all"));
    let do_agy = matches!(target, Some("agy") | Some("all"));

    if do_agy {
        crate::agy_setup::run(sub);
    }
    if do_claude {
        match sub {
            Some("install") => moonlight_control::install_hooks(),
            Some("uninstall") => moonlight_control::uninstall_hooks(),
            Some("status") | None => moonlight_control::print_hook_status(),
            Some(other) => {
                eprintln!("unknown hooks subcommand: {other}");
                eprintln!("usage: moonlight hooks [install|uninstall|status] [claude|agy|all]");
            }
        }
    }
}
