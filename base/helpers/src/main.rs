//! Base's desktop helpers, one binary for all four: the unlock helper, the
//! launcher, the shell's connection to the compositor, and Ctrl+Alt+Del. See
//! README.md.
//!
//! `unsafe` code lives in `sys` and in `crypt`, libcrypt's binding, alone.
#![deny(unsafe_code)]

#[allow(unsafe_code)]
mod crypt;
mod ctrl_alt_del;
mod launcher;
mod request;
mod shell_connect;
#[allow(unsafe_code)]
mod sys;
mod unlock;

use std::process::ExitCode;

/// Base's build gives the person's account this user ID whatever its name
/// (void_base.PERSON_UID): the unlock helper and the launcher find it by it.
pub const PERSON_UID: u32 = 1003;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Helper {
    Unlock,
    Launcher,
    ShellConnect,
    CtrlAltDel,
}

const HELPERS: [(&str, Helper); 4] = [
    ("unlock", Helper::Unlock),
    ("launcher", Helper::Launcher),
    ("shell-connect", Helper::ShellConnect),
    ("ctrl-alt-del", Helper::CtrlAltDel),
];

fn named(name: &str) -> Option<Helper> {
    HELPERS.iter().find(|(n, _)| *n == name).map(|(_, helper)| *helper)
}

/// The helper the command line asks for, and its own arguments. Base installs
/// each as `crowbar-<helper>`, a link to this binary, so the program's name
/// says which; otherwise the first argument does.
fn helper(args: &[String]) -> Option<(Helper, &[String])> {
    let program = std::path::Path::new(args.first()?).file_name()?.to_str()?;
    if let Some(helper) = program.strip_prefix("crowbar-").and_then(named) {
        return Some((helper, &args[1..]));
    }
    Some((named(args.get(1)?)?, &args[2..]))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let Some((helper, rest)) = helper(&args) else {
        eprintln!("usage: crowbar-helpers {{unlock | launcher | shell-connect | ctrl-alt-del}} [ARGS]");
        return ExitCode::from(2);
    };
    let result = match helper {
        Helper::Unlock => unlock::main(rest),
        Helper::Launcher => launcher::main(rest),
        Helper::ShellConnect => shell_connect::main(rest),
        Helper::CtrlAltDel => ctrl_alt_del::main(rest),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let name = HELPERS.iter().find(|(_, h)| *h == helper).map_or("", |(n, _)| n);
            eprintln!("crowbar-{name}: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_helper_is_named_by_the_command_or_by_the_first_argument() {
        let unlock = strings(&["/usr/local/bin/crowbar-unlock", "lock"]);
        assert_eq!(helper(&unlock), Some((Helper::Unlock, &unlock[1..])));
        let launcher = strings(&["crowbar-launcher"]);
        assert_eq!(helper(&launcher), Some((Helper::Launcher, &launcher[1..])));
        let named = strings(&["/usr/local/bin/crowbar-helpers", "unlock", "hash", "x"]);
        assert_eq!(helper(&named), Some((Helper::Unlock, &named[2..])));
        let connect = strings(&["crowbar-helpers", "shell-connect"]);
        assert_eq!(helper(&connect), Some((Helper::ShellConnect, &connect[2..])));
        let chord = strings(&["crowbar-ctrl-alt-del"]);
        assert_eq!(helper(&chord), Some((Helper::CtrlAltDel, &chord[1..])));
    }

    #[test]
    fn anything_else_names_no_helper() {
        for args in [
            vec![],
            strings(&["crowbar-helpers"]),
            strings(&["crowbar-helpers", "reboot"]),
            strings(&["crowbar-reboot"]),
            strings(&["unlock"]),
        ] {
            assert_eq!(helper(&args), None, "{args:?}");
        }
    }
}
