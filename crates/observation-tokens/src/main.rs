//! Writes the design tokens as CSS to the path given (`web/tokens.css`, which
//! `web/index.html` inlines). Run by trunk's pre_build hook; see `Trunk.toml`.

use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(path) = std::env::args_os().nth(1) else {
        eprintln!("usage: token-css <out.css>");
        return ExitCode::FAILURE;
    };
    let css = observation_tokens::css();
    // Leave an up-to-date file alone so `trunk serve` sees no change.
    if std::fs::read_to_string(&path).is_ok_and(|old| old == css) {
        return ExitCode::SUCCESS;
    }
    match std::fs::write(&path, css) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("token-css: cannot write {}: {e}", path.to_string_lossy());
            ExitCode::FAILURE
        }
    }
}
