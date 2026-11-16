//! A small Wayland client for looking at a compositor from outside.

use std::{io::Write, process::Command, time::Instant};

use crowbar_compositor::probe::{self, Anchor, Client, Error, KeyEvent, KeyboardInteractivity, Layer, LayerRequest};

const USAGE: &str = "\
crowbar-probe: an ordinary Wayland client for testing a compositor.

  globals            List the globals this client is offered.
  keylog [TITLE]     Open a window and print every keyboard event it gets.
  layer              Try to create a layer surface and say what happened.
  toplevels          List other clients' windows, if the compositor tells.
  security-context CMD ARGS
                     Register a security context and run CMD inside it.
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("globals") => globals(),
        Some("keylog") => keylog(args.get(1).map(String::as_str).unwrap_or("crowbar-probe keylog")),
        Some("layer") => layer(),
        Some("toplevels") => toplevels(),
        Some("security-context") if args.len() > 1 => security_context(&args[1..]),
        _ => {
            eprint!("{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(error) = result {
        eprintln!("crowbar-probe: {error}");
        std::process::exit(1);
    }
}

fn client() -> Result<Client, Error> {
    Client::new(probe::connect_env()?)
}

fn globals() -> Result<(), Error> {
    let client = client()?;
    let mut globals = client.state.globals.clone();
    globals.sort_by(|a, b| a.interface.cmp(&b.interface));
    for global in globals {
        println!("{} {}", global.interface, global.version);
    }
    Ok(())
}

fn keylog(title: &str) -> Result<(), Error> {
    let start = Instant::now();
    let mut client = client()?;
    client.listen_to_keyboard()?;
    let _window = client.open_toplevel(title, "org.crowbaros.Probe", (480, 320), 0xff1e_6f8c)?;
    let mut out = std::io::stdout();
    writeln!(out, "{:8.3} window mapped", start.elapsed().as_secs_f64())?;
    out.flush()?;
    let mut first = true;
    while !client.state.closed {
        if !first {
            client.queue.blocking_dispatch(&mut client.state)?;
        }
        first = false;
        for scale in client.state.buffer_scales.drain(..) {
            writeln!(
                out,
                "{:8.3} preferred_buffer_scale {scale}",
                start.elapsed().as_secs_f64()
            )?;
        }
        for event in client.state.keys.drain(..) {
            let text = match event {
                KeyEvent::Enter { held } => format!("enter held={held:?}"),
                KeyEvent::Leave => "leave".into(),
                KeyEvent::Key { key, pressed } => {
                    format!("key {key} {}", if pressed { "pressed" } else { "released" })
                }
                KeyEvent::Modifiers { depressed } => format!("modifiers depressed={depressed:#x}"),
            };
            writeln!(out, "{:8.3} {text}", start.elapsed().as_secs_f64())?;
        }
        out.flush()?;
    }
    Ok(())
}

fn toplevels() -> Result<(), Error> {
    for toplevel in client()?.foreign_toplevels()? {
        println!("{}\t{}\t{}", toplevel.identifier, toplevel.app_id, toplevel.title);
    }
    Ok(())
}

fn layer() -> Result<(), Error> {
    let mut client = client()?;
    let window = client.open_layer(
        LayerRequest {
            namespace: "crowbar-probe",
            layer: Layer::Overlay,
            anchor: Anchor::Top | Anchor::Left | Anchor::Right,
            size: (0, 60),
            exclusive_zone: 60,
            keyboard: KeyboardInteractivity::Exclusive,
        },
        0xffc0_2020,
    )?;
    println!("layer surface created, size {:?}", window.size);
    Ok(())
}

fn security_context(command: &[String]) -> Result<(), Error> {
    let mut client = client()?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR is not set")?;
    let path = std::path::PathBuf::from(runtime).join(format!("crowbar-security-context-{}", std::process::id()));
    let listener = client.security_context(&path)?;
    let status = Command::new(&command[0])
        .args(&command[1..])
        .env("WAYLAND_DISPLAY", &listener.path)
        .env_remove("WAYLAND_SOCKET")
        .status()?;
    let _ = std::fs::remove_file(&listener.path);
    std::process::exit(status.code().unwrap_or(1));
}
