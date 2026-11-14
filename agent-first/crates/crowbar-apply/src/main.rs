//! `crowbar`: the Milestone 1 command line for Crowbar State.

use crowbar_apply::bootmenu::MenuOptions;
use crowbar_apply::engine::{Engine, STATE_PATH, load_state};
use crowbar_apply::generations::{Layout, parse_cmdline_generation};
use crowbar_apply::system::{Btrfs, Xbps, run};
use crowbar_apply::{Result, err, ledger, timefmt};
use crowbar_state::State;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

const USAGE: &str = "\
usage: crowbar [--top DIR] [--root DIR] [--from ID] <command>

commands:
  plan <state.toml>        show what apply would change, compared with the booted system
  apply <state.toml>       build a new generation from the booted one; make it the boot default
  generations              list generations, the boot default, and the booted one
  rollback [ID]            make ID (default: the booted generation) the boot default
  drift                    compare the booted system with the state it was built from
  ledger                   print the history of applies and rollbacks
  render-boot-menu         regenerate grub.cfg from the generation records
  init-image <state.toml>  image build only: register --top/gens/1 as generation 1

options:
  --top DIR    Btrfs top level holding gens/ and crowbar/ (default: mount it at /run/crowbar/top)
  --root DIR   system root to read for plan/drift (default: /)
  --from ID    generation to build from (default: the booted one, from /proc/cmdline)
";

const DEFAULT_TOP: &str = "/run/crowbar/top";
const ROOT_LABEL: &str = "crowbar-root";

struct Args {
    top: Option<PathBuf>,
    root: PathBuf,
    from: Option<u32>,
    command: Vec<String>,
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        top: None,
        root: PathBuf::from("/"),
        from: None,
        command: Vec::new(),
    };
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        let mut value = |name: &str| {
            iter.next()
                .ok_or_else(|| crowbar_apply::Error(format!("{name} needs a value")))
        };
        match arg.as_str() {
            "--top" => args.top = Some(PathBuf::from(value("--top")?)),
            "--root" => args.root = PathBuf::from(value("--root")?),
            "--from" => {
                let v = value("--from")?;
                args.from = Some(
                    v.parse()
                        .map_err(|_| crowbar_apply::Error(format!("--from {v} is not a generation id")))?,
                );
            }
            "-h" | "--help" => args.command = vec!["help".into()],
            _ => args.command.push(arg),
        }
    }
    Ok(args)
}

/// Mount the Btrfs top level (subvolid 5) if it is not already reachable.
fn ensure_top(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(top) = explicit {
        return Ok(top);
    }
    let top = PathBuf::from(DEFAULT_TOP);
    if !top.join("crowbar").is_dir() {
        std::fs::create_dir_all(&top)?;
        run(Command::new("mount")
            .args(["-o", "subvolid=5"])
            .arg(format!("LABEL={ROOT_LABEL}"))
            .arg(&top))?;
    }
    Ok(top)
}

fn booted_generation() -> Option<u32> {
    std::fs::read_to_string("/proc/cmdline")
        .ok()
        .as_deref()
        .and_then(parse_cmdline_generation)
}

fn read_state(path: &str) -> Result<(State, String)> {
    let text = std::fs::read_to_string(path).map_err(|e| crowbar_apply::Error(format!("cannot read {path}: {e}")))?;
    Ok((State::from_toml_str(&text)?, text))
}

fn engine(top: PathBuf) -> Engine<'static> {
    Engine {
        layout: Layout::new(top),
        pkgs: &Xbps,
        snap: &Btrfs,
        menu: MenuOptions {
            root_label: ROOT_LABEL.into(),
            ..MenuOptions::default()
        },
        actor: std::env::var("CROWBAR_ACTOR").unwrap_or_else(|_| "user".into()),
        now: timefmt::now_utc,
    }
}

fn print_changes(changes: &[crowbar_apply::plan::Change]) {
    if changes.is_empty() {
        println!("No changes: the system already matches the state.");
        return;
    }
    for change in changes {
        let flag = if change.affects_boot() { "  ! " } else { "  + " };
        println!("{flag}{change}");
    }
    if changes.iter().any(|c| c.affects_boot()) {
        println!("  (! = can affect whether the next boot succeeds; the previous generation stays in the boot menu)");
    }
}

fn real_main() -> Result<ExitCode> {
    let args = parse_args()?;
    let command: Vec<&str> = args.command.iter().map(String::as_str).collect();
    match command.as_slice() {
        ["plan", state] => {
            let (desired, _) = read_state(state)?;
            let e = engine(args.top.unwrap_or_else(|| PathBuf::from(DEFAULT_TOP)));
            print_changes(&e.plan(&desired, &args.root)?);
        }
        ["drift"] => {
            let Some(recorded) = load_state(&args.root)? else {
                return err(format!("{} has no {STATE_PATH}", args.root.display()));
            };
            let e = engine(args.top.unwrap_or_else(|| PathBuf::from(DEFAULT_TOP)));
            let changes = e.plan(&recorded, &args.root)?;
            if changes.is_empty() {
                println!("No drift: the system matches {STATE_PATH}.");
            } else {
                println!("Drift: applying {STATE_PATH} again would change:");
                print_changes(&changes);
                return Ok(ExitCode::from(2));
            }
        }
        ["apply", state] => {
            let (desired, text) = read_state(state)?;
            let from = args
                .from
                .or_else(booted_generation)
                .ok_or_else(|| crowbar_apply::Error("cannot tell which generation is booted; pass --from ID".into()))?;
            let e = engine(ensure_top(args.top)?);
            let report = e.apply(&desired, &text, from)?;
            print_changes(&report.changes);
            match report.to {
                Some(to) => println!(
                    "Built generation {to} from generation {from}; it is now the boot default. Reboot to use it."
                ),
                None => println!("Nothing to build."),
            }
        }
        ["generations"] => {
            let e = engine(ensure_top(args.top)?);
            let default = e.layout.read_default()?;
            let booted = booted_generation();
            for g in e.layout.list()? {
                let mut tags = Vec::new();
                if Some(g.id) == default {
                    tags.push("default");
                }
                if Some(g.id) == booted {
                    tags.push("booted");
                }
                let parent = g.parent.map_or_else(|| "-".into(), |p| p.to_string());
                println!(
                    "{:>4}  parent {:>3}  {}  {}  {}",
                    g.id,
                    parent,
                    g.created,
                    g.summary,
                    tags.join(",")
                );
            }
        }
        ["rollback", rest @ ..] => {
            let e = engine(ensure_top(args.top)?);
            let booted = booted_generation();
            let target = match rest {
                [id] => id
                    .parse()
                    .map_err(|_| crowbar_apply::Error(format!("{id} is not a generation id")))?,
                [] => match (booted, e.layout.read_default()?) {
                    (Some(b), Some(d)) if b != d => b,
                    (Some(b), _) => {
                        return err(format!(
                            "generation {b} is booted and already the default; name the generation to roll back to"
                        ));
                    }
                    (None, _) => {
                        return err("cannot tell which generation is booted; name the generation to roll back to");
                    }
                },
                _ => return err(USAGE),
            };
            e.rollback(target, booted)?;
            println!("Generation {target} is now the boot default.");
        }
        ["ledger"] => {
            let e = engine(ensure_top(args.top)?);
            for entry in ledger::read(&e.layout.ledger_path())? {
                println!(
                    "{}  {:<8} {:<7} gen {:<4} {}",
                    entry.time,
                    entry.action,
                    entry.outcome,
                    entry.generation.map_or_else(|| "-".into(), |g| g.to_string()),
                    entry.detail
                );
            }
        }
        ["render-boot-menu"] => engine(ensure_top(args.top)?).render_menu()?,
        ["init-image", state] => {
            let Some(top) = args.top else {
                return err("init-image needs --top");
            };
            let (_, text) = read_state(state)?;
            let g = engine(top.clone()).init(&text)?;
            println!(
                "Registered generation 1 ({} / {}) under {}",
                g.kernel,
                g.initrd,
                top.display()
            );
        }
        ["help"] | [] => print!("{USAGE}"),
        _ => return err(USAGE),
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    match real_main() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("crowbar: {e}");
            ExitCode::FAILURE
        }
    }
}
