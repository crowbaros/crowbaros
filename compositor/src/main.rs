use crowbar_compositor::State;
use smithay::reexports::{calloop::EventLoop, wayland_server::Display};

const USAGE: &str = "\
crowbar-compositor: spike for issue 3. Throwaway code.

  --backend winit|tty   Draw into a window on a host session, or take a TTY.
                        Default: winit when WAYLAND_DISPLAY or DISPLAY is set.
  --shell 'CMD ARGS'    Start the shell. It gets its own connected socket and
                        is the only client that sees layer-shell.
  --spawn 'CMD ARGS'    Start an ordinary client. May be repeated.
  --scale NAME=FACTOR   Scale for the output with this name. May be repeated.
                        The winit output is named 'winit'.
";

struct Args {
    backend: String,
    shell: Option<Vec<String>>,
    spawn: Vec<Vec<String>>,
    scales: Vec<(String, f64)>,
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

fn parse_args() -> Result<Args, String> {
    let nested = std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some();
    let mut args = Args {
        backend: if nested { "winit".into() } else { "tty".into() },
        shell: None,
        spawn: Vec::new(),
        scales: Vec::new(),
    };
    let mut rest = std::env::args().skip(1);
    while let Some(flag) = rest.next() {
        let mut value = || rest.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--backend" => args.backend = value()?,
            "--shell" => args.shell = Some(words(&value()?)),
            "--spawn" => args.spawn.push(words(&value()?)),
            "--scale" => {
                let value = value()?;
                let (name, factor) = value.split_once('=').ok_or("--scale takes NAME=FACTOR")?;
                let factor: f64 = factor.parse().map_err(|_| "--scale factor is not a number")?;
                args.scales.push((name.to_owned(), factor));
            }
            "--help" | "-h" => return Err(USAGE.into()),
            other => return Err(format!("unknown argument {other}\n\n{USAGE}")),
        }
    }
    Ok(args)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    init_logging();

    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    let display: Display<State> = Display::new()?;
    let mut state = State::new(&mut event_loop, display);
    state.output_scales = args.scales;

    match args.backend.as_str() {
        "winit" => {
            let scale = state.scale_for("winit");
            crowbar_compositor::winit::init_winit(&mut event_loop, &mut state, scale)?;
        }
        "tty" => crowbar_compositor::tty::init_tty(&mut event_loop, &mut state)?,
        other => return Err(format!("unknown backend {other}").into()),
    }
    tracing::info!(socket = ?state.socket_name, backend = %args.backend, "compositor is up");

    let mut children = Vec::new();
    if let Some(command) = &args.shell {
        children.push(state.spawn_shell(command)?);
    }
    for command in &args.spawn {
        children.push(state.spawn_ordinary(command)?);
    }

    event_loop.run(None, &mut state, |state| {
        let _ = state.display_handle.flush_clients();
    })?;

    for mut child in children {
        let _ = child.kill();
        let _ = child.wait();
    }
    Ok(())
}

fn init_logging() {
    use std::io::IsTerminal;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,crowbar_compositor=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(std::io::stderr)
        .init();
}
