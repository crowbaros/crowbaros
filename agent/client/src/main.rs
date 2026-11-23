//! `crowbar-agent`: the person's way to the Agent from any terminal. It is a
//! client of the Harness's person socket (`agent/README.md`), and the first
//! one: it never starts a Harness, holds no conversation, and keeps no
//! credential. See README.md.
//!
//! `unsafe` code lives in `sys` alone.
#![deny(unsafe_code)]

mod session;
mod sign_in;
#[allow(unsafe_code)]
mod sys;
mod talk;

use session::Session;
use std::process::ExitCode;
use talk::Ended;

const USAGE: &str = "\
usage: crowbar-agent                        talk with the Agent, a line at a time
       crowbar-agent \"<request>\"            one turn, and out
       crowbar-agent --list                 list the conversations
       crowbar-agent --resume ID            go on with a conversation
       crowbar-agent --resume ID \"<request>\"  one more turn of it, and out
       crowbar-agent login [PROVIDER]       sign in to a Model
       crowbar-agent logout [PROVIDER]      sign out
       crowbar-agent status                 the Model the Agent uses, and each sign-in
       crowbar-agent model [PROVIDER/MODEL] list the Models signed in to, or choose one

Ctrl-C stops the turn the Agent is working on. Ctrl-D leaves.";

/// What the command line asks for.
#[derive(Debug, PartialEq)]
enum Asked {
    /// A conversation, a line at a time: a new one, or the one named.
    Talk {
        resumed: Option<String>,
    },
    /// One turn, in a new conversation or the one named.
    Once {
        resumed: Option<String>,
        message: String,
    },
    List,
    Login {
        provider: Option<String>,
    },
    Logout {
        provider: Option<String>,
    },
    Status,
    Model {
        chosen: Option<String>,
    },
    Help,
    Version,
}

/// Reads the command line, without the program's name. `login`, `logout`,
/// `status`, and `model` are commands only as the first word: a request
/// that starts with one of them is put in quotes with the rest of it, or
/// after `--`.
fn asked(args: &[String]) -> Result<Asked, String> {
    let (mut resumed, mut list, mut words) = (None, false, Vec::new());
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        // An option comes before the request. A dash inside a request is the request's.
        if !words.is_empty() {
            words.push(arg.clone());
            continue;
        }
        match arg.as_str() {
            "--help" | "-h" => return Ok(Asked::Help),
            "--version" => return Ok(Asked::Version),
            "--list" => list = true,
            "--resume" => {
                resumed = Some(
                    rest.next()
                        .ok_or("--resume needs a conversation's ID, as --list shows it")?
                        .clone(),
                );
            }
            "--" => {
                words.extend(rest.by_ref().cloned());
                return Ok(Asked::Once {
                    resumed,
                    message: words.join(" "),
                });
            }
            option if option.starts_with('-') => return Err(format!("no option {option}")),
            _ => words.push(arg.clone()),
        }
    }
    let one = |what: &str| match &words[1..] {
        [] => Ok(None),
        [one] => Ok(Some(one.clone())),
        _ => Err(format!("{what} takes one name")),
    };
    if list || resumed.is_some() {
        return match (list, words.is_empty()) {
            (true, true) if resumed.is_none() => Ok(Asked::List),
            (true, _) => Err("--list takes nothing else".to_string()),
            (false, true) => Ok(Asked::Talk { resumed }),
            (false, false) => Ok(Asked::Once {
                resumed,
                message: words.join(" "),
            }),
        };
    }
    match words.first().map(String::as_str) {
        None => Ok(Asked::Talk { resumed: None }),
        Some("login") => Ok(Asked::Login {
            provider: one("login")?,
        }),
        Some("logout") => Ok(Asked::Logout {
            provider: one("logout")?,
        }),
        Some("status") if words.len() == 1 => Ok(Asked::Status),
        Some("status") => Err("status takes nothing else".to_string()),
        Some("model") => Ok(Asked::Model { chosen: one("model")? }),
        Some(_) => Ok(Asked::Once {
            resumed: None,
            message: words.join(" "),
        }),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let asked = match asked(&args) {
        Ok(Asked::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Asked::Version) => {
            println!("crowbar-agent {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Ok(asked) => asked,
        Err(why) => {
            eprintln!("crowbar-agent: {why}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let mut session = match Session::connect() {
        Ok(session) => session,
        Err(why) => {
            eprintln!("crowbar-agent: {why}");
            return ExitCode::FAILURE;
        }
    };
    let ended = match &asked {
        Asked::Talk { resumed } => talk::repl(&mut session, resumed.as_deref()),
        Asked::Once { resumed, message } => talk::once(&mut session, resumed.as_deref(), message),
        Asked::List => talk::list(&mut session),
        Asked::Login { provider } => sign_in::login(&mut session, provider.as_deref()),
        Asked::Logout { provider } => sign_in::logout(&mut session, provider.as_deref()),
        Asked::Status => sign_in::status(&mut session),
        Asked::Model { chosen } => sign_in::model(&mut session, chosen.as_deref()),
        Asked::Help | Asked::Version => Ok(Ended::Done),
    };
    match ended {
        Ok(Ended::Done) => ExitCode::SUCCESS,
        Ok(Ended::Failed | Ended::Refused) => ExitCode::FAILURE,
        // As a program that Ctrl-C ended.
        Ok(Ended::Stopped) => ExitCode::from(130),
        Err(error) => {
            // A turn the Agent was working on goes on without this client.
            eprintln!("crowbar-agent: The Harness went away: {error}.");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(line: &[&str]) -> Result<Asked, String> {
        asked(&line.iter().map(|word| word.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn the_command_line_is_read_as_the_usage_has_it() {
        let id = || Some("k3f9q2xz".to_string());
        assert_eq!(read(&[]), Ok(Asked::Talk { resumed: None }));
        assert_eq!(
            read(&["what is my hostname?"]),
            Ok(Asked::Once {
                resumed: None,
                message: "what is my hostname?".into()
            })
        );
        // Words that were not put in quotes are one request all the same.
        assert_eq!(
            read(&["what", "time", "is", "it"]),
            Ok(Asked::Once {
                resumed: None,
                message: "what time is it".into()
            })
        );
        assert_eq!(read(&["--list"]), Ok(Asked::List));
        assert_eq!(read(&["--resume", "k3f9q2xz"]), Ok(Asked::Talk { resumed: id() }));
        assert_eq!(
            read(&["--resume", "k3f9q2xz", "and then?"]),
            Ok(Asked::Once {
                resumed: id(),
                message: "and then?".into()
            })
        );
        assert_eq!(read(&["login"]), Ok(Asked::Login { provider: None }));
        assert_eq!(
            read(&["login", "anthropic"]),
            Ok(Asked::Login {
                provider: Some("anthropic".into())
            })
        );
        assert_eq!(read(&["logout"]), Ok(Asked::Logout { provider: None }));
        assert_eq!(read(&["status"]), Ok(Asked::Status));
        assert_eq!(read(&["model"]), Ok(Asked::Model { chosen: None }));
        assert_eq!(
            read(&["model", "openrouter/anthropic/claude"]),
            Ok(Asked::Model {
                chosen: Some("openrouter/anthropic/claude".into())
            })
        );
        assert_eq!(read(&["--help"]), Ok(Asked::Help));
        assert_eq!(read(&["--version"]), Ok(Asked::Version));
    }

    #[test]
    fn a_request_that_starts_with_a_commands_name_goes_after_two_dashes_or_in_quotes() {
        assert_eq!(
            read(&["--", "status", "of", "my", "disk"]),
            Ok(Asked::Once {
                resumed: None,
                message: "status of my disk".into()
            })
        );
        assert_eq!(
            read(&["status of my disk"]),
            Ok(Asked::Once {
                resumed: None,
                message: "status of my disk".into()
            })
        );
        assert_eq!(
            read(&["--", "--list"]),
            Ok(Asked::Once {
                resumed: None,
                message: "--list".into()
            })
        );
        // A dash inside a request is the request's.
        assert_eq!(
            read(&["what", "does", "-h", "do"]),
            Ok(Asked::Once {
                resumed: None,
                message: "what does -h do".into()
            })
        );
    }

    #[test]
    fn a_command_line_that_asks_for_nothing_the_client_does_is_refused() {
        for line in [
            &["--resume"][..],
            &["--list", "now"],
            &["--list", "--resume", "k3f9q2xz"],
            &["--frobnicate"],
            &["status", "now"],
            &["login", "anthropic", "openai"],
        ] {
            assert!(read(line).is_err(), "{line:?}");
        }
    }
}
