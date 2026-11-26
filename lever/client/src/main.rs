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
       crowbar-agent --archive ID             hide a thread, keeping its record
       crowbar-agent --delete ID              delete a stopped thread and its session
       crowbar-agent --resume ID              go on with a thread
       crowbar-agent --resume ID \"<request>\"  one more turn of it, and out
       crowbar-agent --resume ID --model M    set a thread's Model
       crowbar-agent --model M \"<request>\"    start a thread with Model M
       crowbar-agent --resume ID --effort E   set a thread's effort
       crowbar-agent --resume ID --mode MODE  set a thread's mode
       crowbar-agent --mode MODE \"<request>\"  start a thread in a mode
       crowbar-agent --resume ID --answer R A answer a thread's request R with A
       crowbar-agent --stop ID                stop the working turn of a thread
       crowbar-agent models                   list the Models the harness offers
       crowbar-agent efforts M                list the effort levels of Model M
       crowbar-agent commands                 list the slash commands the harness offers
       crowbar-agent free-models [on|off]     show or switch OpenCode's free hosted Models
       crowbar-agent default-mode [MODE]      show or set the mode new threads start in
       crowbar-agent login                    sign in to a provider with OpenCode
       crowbar-agent status                   Lever's status and available harnesses

Modes are ask-first, full-access, and plan. New threads start in ask-first
or full-access. A command of the harness's is sent as a request that begins
with / and its name. Ctrl-C stops the turn the Agent is working on. Ctrl-D
leaves.";

/// What the command line asks for.
#[derive(Debug, PartialEq)]
enum Command {
    /// A thread, a line at a time: a new one, or the one named.
    Talk {
        resumed: Option<String>,
        model: Option<String>,
        mode: Option<String>,
    },
    /// One turn, in a new thread or the one named.
    Once {
        resumed: Option<String>,
        model: Option<String>,
        mode: Option<String>,
        message: String,
    },
    SetModel {
        thread: String,
        model: String,
    },
    SetMode {
        thread: String,
        mode: String,
    },
    SetEffort {
        thread: String,
        effort: String,
    },
    Answer {
        thread: String,
        request: String,
        answer: String,
    },
    DefaultMode {
        mode: Option<String>,
    },
    List,
    Lifecycle {
        op: String,
        thread: String,
    },
    Stop {
        thread: String,
    },
    Models,
    Efforts {
        model: String,
    },
    Commands,
    FreeModels {
        enabled: Option<bool>,
    },
    Login,
    Status,
    Help,
    Version,
}

/// Reads the command line, without the program's name. `status`, `models`,
/// `efforts`, `commands`, `free-models`, and `login` are commands only as the
/// first word: a request
/// that starts with one goes in quotes with the rest of it, or after `--`.
fn command(args: &[String]) -> Result<Command, String> {
    if let Some(flag @ ("--archive" | "--delete")) = args.first().map(String::as_str) {
        return if args.len() == 2 && !args[1].trim().is_empty() && !args[1].starts_with('-') {
            Ok(Command::Lifecycle {
                op: flag.trim_start_matches('-').into(),
                thread: args[1].clone(),
            })
        } else {
            Err(format!("{flag} needs exactly one thread ID"))
        };
    }
    let (mut resumed, mut model, mut stop, mut list, mut words) = (None, None, None, false, Vec::new());
    let (mut mode, mut answer): (Option<String>, Option<(String, String)>) = (None, None);
    let mut effort: Option<String> = None;
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
            "--model" if model.is_none() => {
                let value = rest
                    .next()
                    .ok_or("--model needs a Model ID, as `crowbar-agent models` shows it")?;
                if value.trim().is_empty() {
                    return Err("--model needs a non-empty Model ID".to_string());
                }
                model = Some(value.clone());
            }
            "--mode" if mode.is_none() => {
                let value = rest
                    .next()
                    .ok_or("--mode needs a mode: ask-first, full-access, or plan")?;
                if value.trim().is_empty() {
                    return Err("--mode needs a non-empty mode".to_string());
                }
                mode = Some(value.clone());
            }
            "--effort" if effort.is_none() => {
                let value = rest
                    .next()
                    .ok_or("--effort needs an effort, as `crowbar-agent efforts` shows them")?;
                if value.trim().is_empty() {
                    return Err("--effort needs a non-empty effort".to_string());
                }
                effort = Some(value.clone());
            }
            "--answer" if answer.is_none() => {
                let request = rest
                    .next()
                    .ok_or("--answer needs a request's ID and an answer, as the request shows them")?;
                let chosen = rest.next().ok_or("--answer needs an answer after the request's ID")?;
                answer = Some((request.clone(), chosen.clone()));
            }
            "--stop" if stop.is_none() => {
                stop = Some(
                    rest.next()
                        .ok_or("--stop needs a thread's ID, as --list shows it")?
                        .clone(),
                );
            }
            "--" => {
                if effort.is_some() {
                    return Err("--effort takes nothing but --resume".to_string());
                }
                if list || stop.is_some() || answer.is_some() {
                    return Err("-- cannot be combined with --list, --stop, or --answer".to_string());
                }
                words.extend(rest.by_ref().cloned());
                if words.iter().all(|word| word.trim().is_empty()) {
                    return Err("-- needs a request after it".to_string());
                }
                return Ok(Command::Once {
                    resumed,
                    model,
                    mode,
                    message: words.join(" "),
                });
            }
            "--list" | "--resume" | "--model" | "--mode" | "--effort" | "--answer" | "--stop" => {
                return Err(format!("{arg} is given twice"));
            }
            option if option.starts_with('-') => return Err(format!("no option {option}")),
            _ => words.push(arg.clone()),
        }
    }
    if list
        || stop.is_some()
        || resumed.is_some()
        || model.is_some()
        || mode.is_some()
        || effort.is_some()
        || answer.is_some()
    {
        let alone = |what: &str| -> Result<(), String> {
            if resumed.is_none()
                && model.is_none()
                && mode.is_none()
                && effort.is_none()
                && answer.is_none()
                && words.is_empty()
            {
                Ok(())
            } else {
                Err(format!("{what} takes nothing else"))
            }
        };
        if list {
            alone("--list")?;
            return if stop.is_none() {
                Ok(Command::List)
            } else {
                Err("--list takes nothing else".to_string())
            };
        }
        if let Some(thread) = stop {
            alone("--stop")?;
            return Ok(Command::Stop { thread });
        }
        if let Some(level) = effort {
            let Some(thread) = resumed else {
                return Err("--effort needs --resume ID, the thread to set".to_string());
            };
            return if model.is_none() && mode.is_none() && answer.is_none() && words.is_empty() {
                Ok(Command::SetEffort { thread, effort: level })
            } else {
                Err("--effort takes nothing but --resume".to_string())
            };
        }
        if let Some((request, chosen)) = answer {
            let Some(thread) = resumed else {
                return Err("--answer needs --resume ID, the request's thread".to_string());
            };
            return if model.is_none() && mode.is_none() && words.is_empty() {
                Ok(Command::Answer {
                    thread,
                    request,
                    answer: chosen,
                })
            } else {
                Err("--answer takes nothing but --resume".to_string())
            };
        }
        if let Some(thread) = &resumed
            && words.is_empty()
        {
            match (&model, &mode) {
                (Some(_), Some(_)) => return Err("--model and --mode are set one at a time".to_string()),
                (Some(chosen), None) => {
                    return Ok(Command::SetModel {
                        thread: thread.clone(),
                        model: chosen.clone(),
                    });
                }
                (None, Some(chosen)) => {
                    return Ok(Command::SetMode {
                        thread: thread.clone(),
                        mode: chosen.clone(),
                    });
                }
                (None, None) => {}
            }
        }
        return if words.is_empty() {
            Ok(Command::Talk { resumed, model, mode })
        } else {
            Ok(Command::Once {
                resumed,
                model,
                mode,
                message: words.join(" "),
            })
        };
    }
    match words.first().map(String::as_str) {
        None => Ok(Command::Talk {
            resumed: None,
            model: None,
            mode: None,
        }),
        Some("status") if words.len() == 1 => Ok(Command::Status),
        Some("status") => Err("status takes nothing else".to_string()),
        Some("models") if words.len() == 1 => Ok(Command::Models),
        Some("models") => Err("models takes nothing else".to_string()),
        Some("efforts") if words.len() == 2 => Ok(Command::Efforts {
            model: words[1].clone(),
        }),
        Some("efforts") => Err("efforts takes a Model ID, as `crowbar-agent models` shows it".to_string()),
        Some("commands") if words.len() == 1 => Ok(Command::Commands),
        Some("commands") => Err("commands takes nothing else".to_string()),
        Some("login") if words.len() == 1 => Ok(Command::Login),
        Some("login") => Err("login takes nothing else".to_string()),
        Some("free-models") if words.len() == 1 => Ok(Command::FreeModels { enabled: None }),
        Some("free-models") if words.len() == 2 => match words[1].as_str() {
            "on" => Ok(Command::FreeModels { enabled: Some(true) }),
            "off" => Ok(Command::FreeModels { enabled: Some(false) }),
            other => Err(format!("free-models takes `on` or `off`, not `{other}`")),
        },
        Some("free-models") => Err("free-models takes at most `on` or `off`".to_string()),
        Some("default-mode") if words.len() == 1 => Ok(Command::DefaultMode { mode: None }),
        Some("default-mode") if words.len() == 2 => Ok(Command::DefaultMode {
            mode: Some(words[1].clone()),
        }),
        Some("default-mode") => Err("default-mode takes at most a mode".to_string()),
        Some(_) => Ok(Command::Once {
            resumed: None,
            model: None,
            mode: None,
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
        Ok(Command::Login) => return talk::login(),
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
        Command::Talk { resumed, model, mode } => {
            talk::repl(&mut lever, resumed.as_deref(), model.as_deref(), mode.as_deref())
        }
        Command::Once {
            resumed,
            model,
            mode,
            message,
        } => talk::once(
            &mut lever,
            resumed.as_deref(),
            model.as_deref(),
            mode.as_deref(),
            message,
        ),
        Command::SetModel { thread, model } => talk::set_model(&mut lever, thread, model),
        Command::SetMode { thread, mode } => talk::set_mode(&mut lever, thread, mode),
        Command::SetEffort { thread, effort } => talk::set_effort(&mut lever, thread, effort),
        Command::Answer {
            thread,
            request,
            answer,
        } => talk::answer(&mut lever, thread, request, answer),
        Command::DefaultMode { mode } => talk::default_mode(&mut lever, mode.as_deref()),
        Command::Lifecycle { op, thread } => talk::lifecycle(&mut lever, op, thread),
        Command::List => talk::list(&mut lever),
        Command::Stop { thread } => talk::stop(&mut lever, thread),
        Command::Models => talk::models(&mut lever),
        Command::Efforts { model } => talk::efforts(&mut lever, model),
        Command::Commands => talk::commands(&mut lever),
        Command::FreeModels { enabled } => talk::free_models(&mut lever, *enabled),
        Command::Status => talk::status(&mut lever),
        Command::Login | Command::Help | Command::Version => Ok(Ended::Done),
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
    fn archive_and_delete_take_exactly_one_thread() {
        for op in ["archive", "delete"] {
            let flag = format!("--{op}");
            assert_eq!(
                read(&[&flag, "t-example"]),
                Ok(Command::Lifecycle {
                    op: op.into(),
                    thread: "t-example".into()
                })
            );
            assert!(read(&[&flag]).is_err());
            assert!(read(&[&flag, "t-example", "extra"]).is_err());
            assert!(read(&[&flag, "--list"]).is_err());
        }
    }

    #[test]
    fn the_command_line_is_read_as_the_usage_has_it() {
        let id = || Some("t-k3f9q2xz".to_string());
        assert_eq!(
            read(&[]),
            Ok(Command::Talk {
                resumed: None,
                model: None,
                mode: None,
            })
        );
        assert_eq!(
            read(&["what is my hostname?"]),
            Ok(Command::Once {
                resumed: None,
                model: None,
                mode: None,
                message: "what is my hostname?".into()
            })
        );
        // Words that were not put in quotes are one request all the same.
        assert_eq!(
            read(&["what", "time", "is", "it"]),
            Ok(Command::Once {
                resumed: None,
                model: None,
                mode: None,
                message: "what time is it".into()
            })
        );
        assert_eq!(read(&["--list"]), Ok(Command::List));
        assert_eq!(
            read(&["--resume", "t-k3f9q2xz"]),
            Ok(Command::Talk {
                resumed: id(),
                model: None,
                mode: None,
            })
        );
        assert_eq!(
            read(&["--resume", "t-k3f9q2xz", "and then?"]),
            Ok(Command::Once {
                resumed: id(),
                model: None,
                mode: None,
                message: "and then?".into()
            })
        );
        assert_eq!(
            read(&["--resume", "t-k3f9q2xz", "--model", "stub/stub-2"]),
            Ok(Command::SetModel {
                thread: "t-k3f9q2xz".into(),
                model: "stub/stub-2".into(),
            })
        );
        assert_eq!(
            read(&["--model", "stub/stub-2", "hello"]),
            Ok(Command::Once {
                resumed: None,
                model: Some("stub/stub-2".into()),
                mode: None,
                message: "hello".into(),
            })
        );
        assert_eq!(
            read(&["--stop", "t-k3f9q2xz"]),
            Ok(Command::Stop {
                thread: "t-k3f9q2xz".into()
            })
        );
        assert_eq!(read(&["models"]), Ok(Command::Models));
        assert_eq!(
            read(&["efforts", "stub/stub-1"]),
            Ok(Command::Efforts {
                model: "stub/stub-1".into()
            })
        );
        assert_eq!(read(&["commands"]), Ok(Command::Commands));
        assert_eq!(
            read(&["--resume", "t-k3f9q2xz", "--effort", "high"]),
            Ok(Command::SetEffort {
                thread: "t-k3f9q2xz".into(),
                effort: "high".into()
            })
        );
        assert!(read(&["--effort", "high"]).is_err());
        assert!(read(&["--resume", "t-k3f9q2xz", "--effort", "high", "and a request"]).is_err());
        assert!(read(&["efforts"]).is_err());
        assert_eq!(read(&["free-models"]), Ok(Command::FreeModels { enabled: None }));
        assert_eq!(
            read(&["free-models", "on"]),
            Ok(Command::FreeModels { enabled: Some(true) })
        );
        assert_eq!(
            read(&["free-models", "off"]),
            Ok(Command::FreeModels { enabled: Some(false) })
        );
        assert_eq!(read(&["login"]), Ok(Command::Login));
        assert_eq!(read(&["status"]), Ok(Command::Status));
        assert_eq!(read(&["--help"]), Ok(Command::Help));
        assert_eq!(read(&["--version"]), Ok(Command::Version));
        assert_eq!(
            read(&["--resume", "t-k3f9q2xz", "--mode", "full-access"]),
            Ok(Command::SetMode {
                thread: "t-k3f9q2xz".into(),
                mode: "full-access".into(),
            })
        );
        assert_eq!(
            read(&["--mode", "plan", "look around"]),
            Ok(Command::Once {
                resumed: None,
                model: None,
                mode: Some("plan".into()),
                message: "look around".into(),
            })
        );
        assert_eq!(
            read(&["--mode", "full-access"]),
            Ok(Command::Talk {
                resumed: None,
                model: None,
                mode: Some("full-access".into()),
            })
        );
        assert_eq!(
            read(&["--resume", "t-k3f9q2xz", "--answer", "r-1a2b3c4d5e6f", "once"]),
            Ok(Command::Answer {
                thread: "t-k3f9q2xz".into(),
                request: "r-1a2b3c4d5e6f".into(),
                answer: "once".into(),
            })
        );
        assert_eq!(read(&["default-mode"]), Ok(Command::DefaultMode { mode: None }));
        assert_eq!(
            read(&["default-mode", "full-access"]),
            Ok(Command::DefaultMode {
                mode: Some("full-access".into())
            })
        );
    }

    #[test]
    fn a_request_that_starts_with_a_commands_name_goes_after_two_dashes_or_in_quotes() {
        assert_eq!(
            read(&["--", "status", "of", "my", "disk"]),
            Ok(Command::Once {
                resumed: None,
                model: None,
                mode: None,
                message: "status of my disk".into()
            })
        );
        assert_eq!(
            read(&["status of my disk"]),
            Ok(Command::Once {
                resumed: None,
                model: None,
                mode: None,
                message: "status of my disk".into()
            })
        );
        assert_eq!(
            read(&["--", "login", "to", "what"]),
            Ok(Command::Once {
                resumed: None,
                model: None,
                mode: None,
                message: "login to what".into()
            })
        );
        assert_eq!(
            read(&["--", "--list"]),
            Ok(Command::Once {
                resumed: None,
                model: None,
                mode: None,
                message: "--list".into()
            })
        );
        // A dash inside a request is the request's.
        assert_eq!(
            read(&["what", "does", "-h", "do"]),
            Ok(Command::Once {
                resumed: None,
                model: None,
                mode: None,
                message: "what does -h do".into()
            })
        );
    }

    #[test]
    fn a_command_line_that_asks_for_nothing_the_client_does_is_refused() {
        for line in [
            &["--resume"][..],
            &["--model"][..],
            &["--model", " "][..],
            &["--stop"][..],
            &["--list", "now"],
            &["--list", "--resume", "t-k3f9q2xz"],
            &["--list", "--model", "stub/stub-1"],
            &["--stop", "t-k3f9q2xz", "extra"],
            &["--stop", "t-k3f9q2xz", "--resume", "t-k3f9q2xz"],
            &["--stop", "t-k3f9q2xz", "--model", "stub/stub-1"],
            &["--frobnicate"],
            &["status", "now"],
            &["models", "now"],
            &["login", "now"],
            &["free-models", "maybe"],
            &["--mode"][..],
            &["--answer", "r-1"][..],
            &["--answer", "r-1", "once"][..],
            &["--resume", "t-k3f9q2xz", "--answer", "r-1", "once", "extra"][..],
            &["--resume", "t-k3f9q2xz", "--model", "m", "--mode", "plan"][..],
            &["--list", "--mode", "plan"][..],
            &["default-mode", "plan", "now"][..],
            &["free-models", "on", "extra"],
            &["--"],
            &["--resume", "t-k3f9q2xz", "--"],
            &["--", " "],
            &["--resume", "t-k3f9q2xz", "--resume", "t-other"],
            &["--model", "a", "--model", "b"],
            &["--stop", "t-k3f9q2xz", "--stop", "t-other"],
            &["--list", "--list"],
        ] {
            assert!(read(line).is_err(), "{line:?}");
        }
    }
}
