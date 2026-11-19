//! xmouse-psnbridge: a PlayStation Remote Play plugin for XMouse.
//!
//! XMouse starts this program with `--xmouse-plugin` and talks to it over stdin/stdout (see
//! `proto.rs`). The Remote Play protocol itself is not implemented here: it comes from
//! chiaki-rust's `chiaki-core`, a Rust port of chiaki-ng's library. This program is the glue
//! between that library and XMouse's plugin protocol.
//!
//! Run by hand it has a few helper commands (`--set-account`, `--show-account`,
//! `--self-test`).

mod account;
mod bridge;
mod consoles;
mod h264;
mod mapping;
mod out;
mod proto;
mod selftest;
mod session;
mod store;

use std::path::PathBuf;
use std::process::ExitCode;

use store::Store;

const USAGE: &str = "\
xmouse-psnbridge: PlayStation Remote Play plugin for XMouse

XMouse starts this program by itself. By hand it understands:

  --set-account <id>   remember the PSN account ID used for pairing
                       (base64 like AbCdEfGhIjk=, or 16 hex digits, or dec:<number>)
  --show-account       print the stored PSN account ID
  --self-test          check this build (no network, no console needed)
  --data-dir <folder>  use this folder for settings instead of the default
                       (the \"data\" folder next to this program, which is where
                       XMouse tells the plugin to keep its files)
  --version            print the version
";

/// Where settings live when XMouse isn't there to say: XMouse passes `<plugin folder>\data`,
/// and the program sits in the plugin folder, so by hand the same folder is used.
fn default_data_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("data")))
        .unwrap_or_else(|| PathBuf::from("data"))
}

/// The value after `flag`, if the flag is there.
fn value_of<'a>(args: &'a [String], flag: &str) -> Option<Option<&'a str>> {
    let at = args.iter().position(|a| a == flag)?;
    Some(args.get(at + 1).map(String::as_str))
}

/// chiaki-core reports through `tracing`. Print that to stderr, which XMouse copies into its
/// log. `XMOUSE_PSNBRIDGE_LOG` (error, warn, info, debug, trace) changes how much.
fn init_logging() {
    let level = match std::env::var("XMOUSE_PSNBRIDGE_LOG").unwrap_or_default().to_ascii_lowercase().as_str() {
        "error" => tracing::Level::ERROR,
        "warn" => tracing::Level::WARN,
        "debug" => tracing::Level::DEBUG,
        "trace" => tracing::Level::TRACE,
        _ => tracing::Level::INFO,
    };
    // stdout belongs to the protocol: every log line must go to stderr.
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .without_time()
        .with_max_level(level)
        .try_init();
}

fn set_account(data_dir: &std::path::Path, text: Option<&str>) -> ExitCode {
    let Some(text) = text else {
        eprintln!("--set-account needs the account ID after it.");
        return ExitCode::from(2);
    };
    let id = match account::parse_account_id(text) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let (mut store, problem) = Store::load(data_dir);
    if let Some(p) = problem {
        eprintln!("warning: {p}");
    }
    store.set_account(&id);
    match store.save(data_dir) {
        Ok(()) => {
            println!("PSN account ID saved in {}", Store::path(data_dir).display());
            println!("(as base64: {})", account::to_base64(&id));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let data_dir = match value_of(&args, "--data-dir") {
        Some(Some(dir)) => PathBuf::from(dir),
        Some(None) => {
            eprintln!("--data-dir needs a folder after it.");
            return ExitCode::from(2);
        }
        None => default_data_dir(),
    };

    if has("--version") {
        println!("xmouse-psnbridge {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if has("--self-test") {
        return if selftest::run() { ExitCode::SUCCESS } else { ExitCode::FAILURE };
    }
    if let Some(text) = value_of(&args, "--set-account") {
        return set_account(&data_dir, text);
    }
    if has("--show-account") {
        return match Store::load(&data_dir).0.account() {
            Some(id) => {
                println!("{}", account::to_base64(&id));
                ExitCode::SUCCESS
            }
            None => {
                eprintln!("No PSN account ID is stored in {}.", Store::path(&data_dir).display());
                ExitCode::FAILURE
            }
        };
    }
    if !has("--xmouse-plugin") {
        print!("{USAGE}");
        return ExitCode::from(2);
    }

    init_logging();
    let (out, writer) = out::spawn(std::io::stdout());
    bridge::serve(std::io::stdin().lock(), &out);
    // Let the last messages (the final status) reach XMouse before the program ends.
    out.close();
    let _ = writer.join();
    ExitCode::SUCCESS
}
