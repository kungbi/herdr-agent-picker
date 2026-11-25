mod completion;
mod config;
mod dialog;
mod hermes_binding;
mod hermes_history;
mod interaction;
mod picker;
mod preview;
mod reminders;
mod sqlite;
mod util;

use std::{
    env,
    io::{self, Write},
    path::Path,
};
use util::Result;

fn run() -> Result<()> {
    // Capture hook observation before doing filesystem or subprocess work.
    let observed = util::now();
    let args: Vec<String> = env::args().skip(1).collect();
    let arg = |n: usize| -> Result<&str> {
        args.get(n)
            .map(String::as_str)
            .ok_or_else(|| "Missing argument".into())
    };
    match args.first().map(String::as_str).unwrap_or("picker") {
        "picker" => picker::main(),
        "open" => util::exec_herdr(&[
            "plugin",
            "pane",
            "open",
            "--plugin",
            "agent-picker",
            "--entrypoint",
            "picker",
        ]),
        "record" => completion::record_event(
            &serde_json::from_str(&env::var("HERDR_PLUGIN_EVENT_JSON")?)?,
            completion::cache_path().as_deref(),
            observed,
        ),
        "alarms" | "list" => reminders::manage(),
        "menu" => reminders::menu(args.get(1).map(String::as_str).unwrap_or("")),
        "--interact" => interaction::open(Path::new(arg(1)?), arg(2)?),
        "worker" => reminders::worker(Path::new(arg(1)?)),
        "check" => util::check(),
        "--version" => {
            println!("herdr-agent-picker {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "--preview" => {
            preview::snapshot(arg(1)?)?;
            Ok(())
        }
        "--live-preview" => {
            preview::live(
                arg(2)?,
                Path::new(arg(1)?),
                args.get(3).map(String::as_str).unwrap_or(""),
            );
            Ok(())
        }
        "--attach-guard" => preview::guard(arg(1)?, arg(2)?.parse()?, arg(3)?.parse()?),
        "--rows" => {
            print!("{}", picker::refresh_rows(Path::new(arg(1)?))?);
            Ok(())
        }
        "--scroll" => picker::scroll(Path::new(arg(1)?), arg(2)?.parse()?, arg(3)?),
        "--focus-preview" => picker::focus_preview(Path::new(arg(1)?), arg(2)?),
        "--refresh" => {
            print!("{}", picker::refresh_action(Path::new(arg(1)?))?);
            Ok(())
        }
        other => Err(format!("Unknown command: {other}").into()),
    }
}
fn main() {
    if let Err(error) = run() {
        eprintln!("Agent picker error: {error}");
        // Match the popup's readable error prompt without blocking hooks.
        let interactive_picker = env::args().nth(1).is_none_or(|mode| mode == "picker");
        if interactive_picker && !util::stopped() && unsafe { libc::isatty(0) } == 1 {
            print!("Press Enter to close");
            let _ = io::stdout().flush();
            let _ = io::stdin().read_line(&mut String::new());
        }
        std::process::exit(1);
    }
}
