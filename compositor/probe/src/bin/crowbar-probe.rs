#![forbid(unsafe_code)]

//! Wayland clients for looking at a compositor from outside.

use std::{io::Write, path::Path, process::Command, time::Instant};

use crowbar_probe::{self as probe, Client, Error, KeyEvent, LayerRequest};

const USAGE: &str = "\
crowbar-probe: Wayland clients for testing a compositor. Never part of Base.

Each connects as any client does: WAYLAND_DISPLAY names the socket, by path or
by a name in XDG_RUNTIME_DIR.

  globals            List the globals this client is offered.
  keylog [TITLE]     Open a window and print every keyboard event it gets.
  layer              Try to create a layer surface and say what happened.
  guess INTERFACE... Ask for each interface by guessing its global number,
                     without being offered it. Fails if any bind is accepted.
  warden PRINCIPAL COMPARTMENT SOCKET [CMD ARGS]
                     Stand in for the Warden: on the Warden's connection,
                     register SOCKET as that compartment's listener. Then run
                     CMD as a client of it, or keep it open until killed.
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("globals") => globals(),
        Some("keylog") => keylog(args.get(1).map(String::as_str).unwrap_or("crowbar-probe keylog")),
        Some("layer") => layer(),
        Some("guess") if args.len() > 1 => guess(&args[1..]),
        Some("warden") if args.len() > 3 => warden(&args[1], &args[2], Path::new(&args[3]), &args[4..]),
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
    Client::connect(&probe::display_path()?)
}

fn globals() -> Result<(), Error> {
    let client = client()?;
    let mut globals = client.globals.clone();
    globals.sort_by(|a, b| a.interface.cmp(&b.interface));
    for global in globals {
        println!("{} v{} (global {})", global.interface, global.version, global.name);
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
    loop {
        client.wait_for_events(std::time::Duration::from_secs(1))?;
        if client.is_closed() {
            return Ok(());
        }
        for event in client.take_keys()? {
            let text = match event {
                KeyEvent::Enter { keys } => format!("enter held={keys:?}"),
                KeyEvent::Leave => "leave".into(),
                KeyEvent::Key { key, pressed } => {
                    format!("key {key} {}", if pressed { "pressed" } else { "released" })
                }
            };
            writeln!(out, "{:8.3} {text}", start.elapsed().as_secs_f64())?;
        }
        out.flush()?;
    }
}

fn layer() -> Result<(), Error> {
    let mut client = client()?;
    // An overlay bar across the top that takes the keyboard on a click.
    let window = client.open_layer(
        LayerRequest {
            namespace: "crowbar-probe",
            layer: 3,
            anchor: 1 | 4 | 8,
            size: (0, 60),
            exclusive_zone: 60,
            keyboard: 1,
        },
        0xffc0_2020,
    )?;
    println!("layer surface created, size {:?}", window.size);
    Ok(())
}

/// How far past the last global this client was told about the guesses go.
const GUESS_PAST: u32 = 16;

fn guess(interfaces: &[String]) -> Result<(), Error> {
    let mut bound = 0;
    for interface in interfaces {
        let accepted = probe::guess_everywhere(&client, interface, GUESS_PAST)?;
        if accepted.is_empty() {
            println!("{interface}: refused at every number it could guess");
        } else {
            println!("{interface}: BOUND at {accepted:?}");
            bound += 1;
        }
    }
    if bound > 0 {
        return Err(format!("{bound} interfaces were bound without being offered").into());
    }
    Ok(())
}

fn warden(principal: &str, compartment: &str, socket: &Path, command: &[String]) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    let mut client = client()?;
    let listener = client.register_compartment(socket, Some(principal), Some(compartment))?;
    // The directory the socket is in decides who reaches it, as the sandbox
    // does for a real compartment.
    std::fs::set_permissions(&listener.path, std::fs::Permissions::from_mode(0o666))?;
    println!("listening at {}", listener.path.display());
    std::io::stdout().flush()?;
    if command.is_empty() {
        // Hold the listener until the compositor goes away, as the Warden
        // holds it for as long as the compartment runs. No deadline: a
        // stand-in that gave up would close the listener under the clients.
        loop {
            client.wait_for_events(std::time::Duration::from_secs(60))?;
            if client.is_closed() {
                return Err("the compositor closed the connection".into());
            }
        }
    }
    let status = Command::new(&command[0])
        .args(&command[1..])
        .env("WAYLAND_DISPLAY", &listener.path)
        .env_remove("WAYLAND_SOCKET")
        .status()?;
    drop(listener);
    std::process::exit(status.code().unwrap_or(1));
}
