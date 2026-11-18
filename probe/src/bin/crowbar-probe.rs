#![forbid(unsafe_code)]

//! Wayland clients for looking at a compositor from outside.

use std::{io::Write, path::Path, process::Command};

use crowbar_probe::{self as probe, Client, Error};

const USAGE: &str = "\
crowbar-probe: Wayland clients for testing a compositor. Never part of Base.

Each connects as any client does: WAYLAND_DISPLAY names the socket, by path or
by a name in XDG_RUNTIME_DIR.

  globals            List the globals this client is offered.
  guess INTERFACE... Ask for each interface by guessing its global number,
                     without being offered it. Fails if any bind is accepted.
  warden PRINCIPAL COMPARTMENT SOCKET [CMD ARGS]
                     Stand in for the Warden: on the Warden's connection,
                     register SOCKET as that compartment's listener. Then run
                     CMD as a client of it, or keep it open until killed.

The window, layer, and keyboard commands return as the compositor gains what
they test.
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("globals") => globals(),
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
        loop {
            client.wait_until(|_| false)?;
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
