//! `crowbar-agent`: the person's way to the Agent from any terminal. It is a
//! client of Lever's person socket (`lever/README.md`): it never starts Lever
//! or a harness, holds no thread of its own, and keeps no credential.
//!
//! `unsafe` code lives in `sys` alone.
#![deny(unsafe_code)]

mod connection;
#[allow(unsafe_code)]
mod sys;
mod talk;

use connection::{Connection, Unconnected};
use std::process::ExitCode;
use talk::Ended;

const USAGE: &str = "\
usage: crowbar-agent                          talk with the Agent, a line at a time
       crowbar-agent \"<request>\"              one turn, and out
       crowbar-agent --list                   list the threads
       crowbar-agent --resume ID              go on with a thread
       crowbar-agent --resume ID \"<request>\"  one more turn of it, and out
       crowbar-agent --stop ID                stop the working turn of a thread
       crowbar-agent status                   Lever's status and available harnesses

Ctrl-C stops the turn the Agent is working on. Ctrl-D leaves.";

/// What the command line asks for.
#[derive(Debug, PartialEq)]
enum Command {
    /// A thread, a line at a time: a new one, or the one named.
    Talk {
        resumed: Option<String>,
    },
    /// One turn, in a new thread or the one named.
    Once {
        resumed: Option<String>,
        message: String,
    },
    List,
    Stop {
        thread: String,
    },
    Status,
    Help,
    Version,
}

/// Reads the command line, without the program's name. `status` is a command
/// only as the first word: a request that starts with it goes in quotes with
/// the rest of it, or after `--`.
fn command(args: &[String]) -> Result<Command, String> {
    let (mut resumed, mut stop, mut list, mut words) = (None, None, false, Vec::new());
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        // An option comes before the request. A dash inside a request is the request's.
        if !words.is_empty() {
            words.push(arg.clone());
            continue;
        }
        match arg.as_str() {
            "--help" | "-h" => return Ok(Command::Help),
            "--version" => return Ok(Command::Version),
            "--list" if !list => list = true,
            "--resume" if resumed.is_none() => {
                resumed = Some(
                    rest.next()
                        .ok_or("--resume needs a thread's ID, as --list shows it")?
                        .clone(),
                );
            }
            "--stop" if stop.is_none() => {
                stop = Some(
                    rest.next()
                        .ok_or("--stop needs a thread's ID, as --list shows it")?
                        .clone(),
                );
            }
            "--" => {
                if list || stop.is_some() {
                    return Err("-- cannot be combined with --list or --stop".to_string());
                }
                words.extend(rest.by_ref().cloned());
                if words.iter().all(|word| word.trim().is_empty()) {
                    return Err("-- needs a request after it".to_string());
                }
                return Ok(Command::Once {
                    resumed,
                    message: words.join(" "),
                });
            }
            "--list" | "--resume" | "--stop" => return Err(format!("{arg} is given twice")),
            option if option.starts_with('-') => return Err(format!("no option {option}")),
            _ => words.push(arg.clone()),
        }
    }
    if list || stop.is_some() || resumed.is_some() {
        if list {
            return if stop.is_none() && resumed.is_none() && words.is_empty() {
                Ok(Command::List)
            } else {
                Err("--list takes nothing else".to_string())
            };
        }
        if let Some(thread) = stop {
            return if resumed.is_none() && words.is_empty() {
                Ok(Command::Stop { thread })
            } else {
                Err("--stop takes nothing else".to_string())
            };
        }
        return if words.is_empty() {
            Ok(Command::Talk { resumed })
        } else {
            Ok(Command::Once {
                resumed,
                message: words.join(" "),
            })
        };
    }
    match words.first().map(String::as_str) {
        None => Ok(Command::Talk { resumed: None }),
        Some("status") if words.len() == 1 => Ok(Command::Status),
        Some("status") => Err("status takes nothing else".to_string()),
        Some(_) => Ok(Command::Once {
            resumed: None,
            message: words.join(" "),
        }),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let asked = match command(&args) {
        Ok(Command::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Command::Version) => {
            println!("crowbar-agent {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Ok(asked) => asked,
        Err(why) => {
            eprintln!("crowbar-agent: {why}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let mut lever = match Connection::connect() {
        Ok(lever) => lever,
        Err(Unconnected::Interrupted) => return ExitCode::from(130),
        Err(Unconnected::Why(why)) => {
            eprintln!("crowbar-agent: {why}");
            return ExitCode::FAILURE;
        }
    };
    let ended = match &asked {
        Command::Talk { resumed } => talk::repl(&mut lever, resumed.as_deref()),
        Command::Once { resumed, message } => talk::once(&mut lever, resumed.as_deref(), message),
        Command::List => talk::list(&mut lever),
        Command::Stop { thread } => talk::stop(&mut lever, thread),
        Command::Status => talk::status(&mut lever),
        Command::Help | Command::Version => Ok(Ended::Done),
    };
    match ended {
        Ok(Ended::Done) => ExitCode::SUCCESS,
        Ok(Ended::Failed | Ended::Refused) => ExitCode::FAILURE,
        // As a program that Ctrl-C ended.
        Ok(Ended::Stopped) => ExitCode::from(130),
        // Ctrl-C while Lever was waited for.
        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => ExitCode::from(130),
        Err(error) => {
            // A turn the Agent was working on goes on without this client.
            eprintln!("crowbar-agent: Lever went away: {error}.");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(line: &[&str]) -> Result<Command, String> {
        command(&line.iter().map(|word| word.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn the_command_line_is_read_as_the_usage_has_it() {
        let id = || Some("t-k3f9q2xz".to_string());
        assert_eq!(read(&[]), Ok(Command::Talk { resumed: None }));
        assert_eq!(
            read(&["what is my hostname?"]),
            Ok(Command::Once {
                resumed: None,
                message: "what is my hostname?".into()
            })
        );
        // Words that were not put in quotes are one request all the same.
        assert_eq!(
            read(&["what", "time", "is", "it"]),
            Ok(Command::Once {
                resumed: None,
                message: "what time is it".into()
            })
        );
        assert_eq!(read(&["--list"]), Ok(Command::List));
        assert_eq!(read(&["--resume", "t-k3f9q2xz"]), Ok(Command::Talk { resumed: id() }));
        assert_eq!(
            read(&["--resume", "t-k3f9q2xz", "and then?"]),
            Ok(Command::Once {
                resumed: id(),
                message: "and then?".into()
            })
        );
        assert_eq!(
            read(&["--stop", "t-k3f9q2xz"]),
            Ok(Command::Stop {
                thread: "t-k3f9q2xz".into()
            })
        );
        assert_eq!(read(&["status"]), Ok(Command::Status));
        assert_eq!(read(&["--help"]), Ok(Command::Help));
        assert_eq!(read(&["--version"]), Ok(Command::Version));
    }

    #[test]
    fn a_request_that_starts_with_a_commands_name_goes_after_two_dashes_or_in_quotes() {
        assert_eq!(
            read(&["--", "status", "of", "my", "disk"]),
            Ok(Command::Once {
                resumed: None,
                message: "status of my disk".into()
            })
        );
        assert_eq!(
            read(&["status of my disk"]),
            Ok(Command::Once {
                resumed: None,
                message: "status of my disk".into()
            })
        );
        assert_eq!(
            read(&["--", "--list"]),
            Ok(Command::Once {
                resumed: None,
                message: "--list".into()
            })
        );
        // A dash inside a request is the request's.
        assert_eq!(
            read(&["what", "does", "-h", "do"]),
            Ok(Command::Once {
                resumed: None,
                message: "what does -h do".into()
            })
        );
    }

    #[test]
    fn a_command_line_that_asks_for_nothing_the_client_does_is_refused() {
        for line in [
            &["--resume"][..],
            &["--stop"][..],
            &["--list", "now"],
            &["--list", "--resume", "t-k3f9q2xz"],
            &["--stop", "t-k3f9q2xz", "extra"],
            &["--stop", "t-k3f9q2xz", "--resume", "t-k3f9q2xz"],
            &["--frobnicate"],
            &["status", "now"],
            &["--"],
            &["--resume", "t-k3f9q2xz", "--"],
            &["--", " "],
            &["--resume", "t-k3f9q2xz", "--resume", "t-other"],
            &["--stop", "t-k3f9q2xz", "--stop", "t-other"],
            &["--list", "--list"],
        ] {
            assert!(read(line).is_err(), "{line:?}");
        }
    }
}
