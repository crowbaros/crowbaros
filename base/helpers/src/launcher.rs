//! The launcher service, `crowbar-launcher`: it starts the apps the shell asks
//! for, as the person.
//!
//! runit starts it as root. It binds its socket in a directory root owns, so
//! no app can take the socket's place, and then becomes the person's account,
//! with its groups, before it reads a request. Every app it starts runs as
//! the person, with their home, at the compositor's socket for apps (ADR
//! 0032), as an app would on any desktop. It takes requests only from the
//! shell's user ID. Its other request reads the person's saved appearance for
//! the shell.

use crate::request;
use crate::sys::{self, Account};
use serde_json::{Value, json};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SOCKET: &str = "/run/crowbar-launch/socket";
const SHELL: &str = "crowbar-shell";
const APPS_LOG: &str = "/var/log/crowbar-apps.log";
/// The compositor's socket for apps, as Base links it into the person's
/// runtime directory.
const WAYLAND_DISPLAY: &str = "wayland-0";
/// Where Void keeps the system's `LANG`.
const LOCALE: &str = "/etc/locale.conf";
const MAX_PREFERENCES: usize = 64 << 10;
/// How long `gio launch` may take to start an app.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(8);

/// Runs a launch command to its end, or says why it failed.
pub type Run<'a> = dyn FnMut(&mut Command) -> Result<(), String> + 'a;

/// The desktop entry at this full path, if it is a file.
pub fn desktop_path(value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if !path.is_absolute() || path.extension().is_none_or(|e| e != "desktop") {
        return Err("Only a desktop entry, by its full path, can be launched".into());
    }
    if !std::fs::metadata(&path).map_err(|e| e.to_string())?.is_file() {
        return Err("A desktop entry must be a file".into());
    }
    Ok(path)
}

/// The person's saved appearance, ~/.config/crowbaros/appearance.json, or
/// None when there is none. No symbolic link is followed on the way, and the
/// file must be a regular one, the person's alone, of at most 64 KiB.
pub fn appearance(home: &Path, uid: u32) -> Result<Option<Value>, String> {
    use std::os::fd::AsRawFd;
    let found = (|| -> io::Result<File> {
        let home = home.to_str().ok_or_else(|| io::Error::other("The home is not UTF-8"))?;
        let mut directory = sys::open_at(libc::AT_FDCWD, home, libc::O_RDONLY | libc::O_DIRECTORY)?;
        for name in [".config", "crowbaros"] {
            directory = sys::open_at(directory.as_raw_fd(), name, libc::O_RDONLY | libc::O_DIRECTORY)?;
        }
        let file = sys::open_at(
            directory.as_raw_fd(),
            "appearance.json",
            libc::O_RDONLY | libc::O_NONBLOCK,
        )?;
        Ok(File::from(file))
    })();
    let file = match found {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        found => found.map_err(|e| e.to_string())?,
    };
    let info = file.metadata().map_err(|e| e.to_string())?;
    if !info.file_type().is_file() || info.uid() != uid || info.nlink() != 1 {
        return Err("Preferences must be a regular file owned only by the person".into());
    }
    let mut data = Vec::new();
    file.take(MAX_PREFERENCES as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    if data.len() > MAX_PREFERENCES {
        return Err("Preferences exceed 64 KiB".into());
    }
    serde_json::from_slice(&data).map(Some).map_err(|e| e.to_string())
}

/// The system's `LANG`, as Void's locale.conf sets it, or C.UTF-8.
pub fn system_locale(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .filter_map(|line| line.split_once('='))
        .find(|(name, value)| name.trim() == "LANG" && !value.trim().is_empty())
        .map(|(_, value)| value.trim().trim_matches('"').to_string())
        .unwrap_or_else(|| "C.UTF-8".into())
}

/// What a login gives the person, and what the desktop adds.
pub fn app_environment(person: &Account, locale: &str) -> Vec<(String, String)> {
    let home = person.dir.display();
    let shell = if person.shell.is_empty() {
        "/bin/sh"
    } else {
        &person.shell
    };
    [
        ("HOME", home.to_string()),
        ("USER", person.name.clone()),
        ("LOGNAME", person.name.clone()),
        ("SHELL", shell.into()),
        (
            "PATH",
            format!("{home}/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/bin:/usr/sbin:/sbin:/bin"),
        ),
        ("LANG", locale.into()),
        ("XDG_RUNTIME_DIR", format!("/run/user/{}", person.uid)),
        ("XDG_CURRENT_DESKTOP", "CrowbarOS".into()),
        ("XDG_SESSION_TYPE", "wayland".into()),
        ("WAYLAND_DISPLAY", WAYLAND_DISPLAY.into()),
        ("BROWSER", "chromium".into()),
        ("XCURSOR_THEME", "Vanilla-DMZ".into()),
        ("XCURSOR_SIZE", "24".into()),
        ("CROWBAR_DISPLAY_SOCKET", "/run/crowbar-displays/socket".into()),
        ("CROWBAR_SESSION", "crowbaros".into()),
        ("RUST_LOG", "info".into()),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value))
    .collect()
}

/// A program as the launcher starts an app: with the person's environment
/// and nothing of this process's, starting in the person's home.
fn app_command(program: &str, person: &Account, locale: &str) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .envs(app_environment(person, locale))
        .current_dir(&person.dir);
    command
}

/// `gio launch` for this entry, as the launcher itself, so as the person,
/// with the person's environment and nothing of the launcher's.
fn launch_command(entry: &Path, person: &Account) -> Command {
    let mut command = app_command("/usr/bin/gio", person, &system_locale(Path::new(LOCALE)));
    command.arg("launch").arg(entry).stdin(Stdio::null());
    command
}

/// `crowbar-launcher as-app PROGRAM [ARGS]`: the program, as the launcher
/// starts an app. It changes no account, so only a process that is already
/// the person's, with user ID `runner`, gets one.
pub fn as_app(command: &[String], runner: u32, person: &Account, locale: &str) -> Result<Command, String> {
    let Some((program, args)) = command.split_first() else {
        return Err("usage: crowbar-launcher as-app PROGRAM [ARGS]".into());
    };
    if runner != person.uid {
        return Err("Only the person's account runs a program as an app".into());
    }
    let mut command = app_command(program, person, locale);
    command.args(args);
    Ok(command)
}

fn dispatch(request: serde_json::Map<String, Value>, person: &Account, run: &mut Run) -> Result<Value, String> {
    let op = request.get("op").and_then(Value::as_str);
    if request.len() == 1 && op == Some("appearance") {
        return Ok(json!({"ok": true, "preferences": appearance(&person.dir, person.uid)?}));
    }
    let (Some("launch"), 2, Some(entry)) = (op, request.len(), request.get("desktop")) else {
        return Err("Unknown request".into());
    };
    let entry = desktop_path(entry.as_str().ok_or("A desktop entry is a path")?)?;
    run(&mut launch_command(&entry, person))?;
    Ok(json!({"ok": true}))
}

/// The reply to one connection from a peer with this user ID. Only the
/// shell's is read.
pub fn answer(peer: u32, shell_uid: u32, stream: &mut impl Read, person: &Account, run: &mut Run) -> Value {
    let reply = (|| {
        if peer != shell_uid {
            return Err("Only the shell may make a request".to_string());
        }
        dispatch(
            request::parse_object(&request::read_line(stream, request::MAX_REQUEST)?)?,
            person,
            run,
        )
    })();
    reply.unwrap_or_else(request::refusal)
}

/// Answers one connection on the launcher's socket.
pub fn serve_one(listener: &UnixListener, shell_uid: u32, person: &Account, run: &mut Run) {
    let Ok((mut connection, _)) = listener.accept() else {
        return;
    };
    let _ = connection.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = connection.set_write_timeout(Some(Duration::from_secs(2)));
    let reply = match sys::peer_uid(&connection) {
        Ok(peer) => answer(peer, shell_uid, &mut connection, person, run),
        Err(error) => request::refusal(error),
    };
    let _ = connection.write_all(format!("{reply}\n").as_bytes());
}

/// Runs a launch in a session of its own, its output to the apps' log, and
/// waits for it to finish, as `gio launch` does once the app has started.
fn run_launch(command: &mut Command) -> Result<(), String> {
    let log = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(APPS_LOG)
        .map_err(|e| e.to_string())?;
    command.stdout(log.try_clone().map_err(|e| e.to_string())?).stderr(log);
    sys::new_session(command);
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let deadline = Instant::now() + LAUNCH_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return if status.success() {
                Ok(())
            } else {
                Err("The application failed to launch".into())
            };
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "The launch took longer than {} seconds",
                LAUNCH_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `crowbar-launcher`, as runit starts it, or `crowbar-launcher as-app`, as
/// the Harness's service does.
pub fn main(args: &[String]) -> Result<(), String> {
    let uid = crate::PERSON_UID;
    if let Some(("as-app", command)) = args.split_first().map(|(first, rest)| (first.as_str(), rest)) {
        let person = sys::require(sys::account_by_uid(uid), &format!("with user ID {uid}"))?;
        let mut command = as_app(command, sys::uid(), &person, &system_locale(Path::new(LOCALE)))?;
        // Only returns when the program could not be run.
        let error = command.exec();
        return Err(format!("{:?}: {error}", command.get_program()));
    }
    if !args.is_empty() {
        return Err("usage: crowbar-launcher [as-app PROGRAM [ARGS]]".into());
    }
    let shell = sys::require(sys::account_by_name(SHELL), SHELL)?;
    let person = sys::require(sys::account_by_uid(uid), &format!("with user ID {uid}"))?;
    // Root's socket in root's directory, which the shell's group may enter:
    // the person, and so every app, may neither replace nor remove it. The
    // peer's user ID is the check that counts.
    let listener = sys::listen_as(Path::new(SOCKET), 0, shell.gid).map_err(|e| e.to_string())?;
    sys::become_account(&person).map_err(|e| format!("The launcher could not become the person: {e}"))?;
    loop {
        serve_one(&listener, shell.uid, &person, &mut run_launch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::tests::TempDir;
    use serde_json::json;
    use std::cell::RefCell;

    const SHELL_UID: u32 = 1001;

    fn person(home: &Path) -> Account {
        Account {
            name: "alex".into(),
            uid: crate::sys::uid(),
            gid: 0,
            dir: home.into(),
            shell: "/bin/bash".into(),
        }
    }

    /// The reply to `line` from `peer`, and each command the launcher ran.
    fn ask(line: &[u8], peer: u32, person: &Account) -> (serde_json::Value, Vec<String>) {
        let ran = RefCell::new(vec![]);
        let reply = answer(peer, SHELL_UID, &mut &line[..], person, &mut |command: &mut Command| {
            ran.borrow_mut().push(format!("{command:?}"));
            Ok(())
        });
        (reply, ran.into_inner())
    }

    struct Home {
        dir: TempDir,
    }

    impl Home {
        fn new() -> Home {
            let dir = TempDir::new();
            std::fs::create_dir_all(dir.0.join(".config/crowbaros")).unwrap();
            Home { dir }
        }
        fn path(&self) -> &Path {
            &self.dir.0
        }
        fn folder(&self) -> PathBuf {
            self.dir.0.join(".config/crowbaros")
        }
        fn read(&self, uid: u32) -> Result<Option<serde_json::Value>, String> {
            appearance(self.path(), uid)
        }
    }

    #[test]
    fn only_the_shell_may_make_a_request() {
        let home = Home::new();
        for peer in [0, 1003, 1000, 1002] {
            let (reply, ran) = ask(
                b"{\"op\":\"launch\",\"desktop\":\"/usr/share/applications/foot.desktop\"}\n",
                peer,
                &person(home.path()),
            );
            assert_eq!(
                reply,
                json!({"ok": false, "error": "Only the shell may make a request"}),
                "{peer}"
            );
            assert!(ran.is_empty());
        }
    }

    #[test]
    fn the_shell_reads_the_persons_appearance() {
        let home = Home::new();
        std::fs::write(home.folder().join("appearance.json"), r#"{"theme":"light"}"#).unwrap();
        let (reply, _) = ask(b"{\"op\":\"appearance\"}\n", SHELL_UID, &person(home.path()));
        assert_eq!(reply, json!({"ok": true, "preferences": {"theme": "light"}}));
    }

    #[test]
    fn a_launch_runs_gio_as_the_launcher_itself_at_the_persons_socket() {
        let home = Home::new();
        let entry = home.path().join("foot.desktop");
        std::fs::write(&entry, "[Desktop Entry]\n").unwrap();
        let line = format!("{}\n", json!({"op": "launch", "desktop": entry}));
        let mut seen = None;
        let reply = answer(
            SHELL_UID,
            SHELL_UID,
            &mut line.as_bytes(),
            &person(home.path()),
            &mut |command: &mut Command| {
                seen = Some((
                    command.get_program().to_owned(),
                    command.get_args().map(ToOwned::to_owned).collect::<Vec<_>>(),
                    command
                        .get_envs()
                        .map(|(k, v)| (k.to_owned(), v.map(ToOwned::to_owned)))
                        .collect::<Vec<_>>(),
                    command.get_current_dir().map(ToOwned::to_owned),
                ));
                Ok(())
            },
        );
        assert_eq!(reply, json!({"ok": true}));
        // As on any desktop: sudo and su work from the terminal, so the
        // launcher sets no other user or group for the app.
        let (program, args, env, dir) = seen.unwrap();
        assert_eq!(program, "/usr/bin/gio");
        assert_eq!(args, ["launch", entry.to_str().unwrap()]);
        assert!(env.contains(&("WAYLAND_DISPLAY".into(), Some("wayland-0".into()))));
        assert_eq!(dir.as_deref(), Some(home.path()));
    }

    #[test]
    fn a_failed_launch_is_reported() {
        let home = Home::new();
        let entry = home.path().join("broken.desktop");
        std::fs::write(&entry, "").unwrap();
        let line = format!("{}\n", json!({"op": "launch", "desktop": entry}));
        let reply = answer(
            SHELL_UID,
            SHELL_UID,
            &mut line.as_bytes(),
            &person(home.path()),
            &mut |_: &mut Command| Err("The application failed to launch".into()),
        );
        assert_eq!(reply, json!({"ok": false, "error": "The application failed to launch"}));
    }

    #[test]
    fn a_malformed_or_unknown_request_is_refused() {
        let home = Home::new();
        let long = [b"x".repeat(5000), b"\n".to_vec()].concat();
        for line in [
            b"{\"op\":\"appearance\"}".as_slice(),
            b"[]\n",
            b"{\"op\":\"run\",\"command\":\"sh\"}\n",
            b"{\"op\":\"launch\",\"desktop\":\"/usr/share/applications/foot.desktop\",\"uid\":0}\n",
            b"{\"op\":\"launch\",\"desktop\":1}\n",
            b"{\"op\":\"appearance\",\"path\":\"/etc/shadow\"}\n",
            &long,
        ] {
            let (reply, ran) = ask(line, SHELL_UID, &person(home.path()));
            assert_eq!(
                reply["ok"],
                false,
                "{}",
                String::from_utf8_lossy(&line[..line.len().min(40)])
            );
            assert!(ran.is_empty());
        }
    }

    #[test]
    fn absent_and_regular_preferences() {
        let home = Home::new();
        assert_eq!(home.read(crate::sys::uid()), Ok(None));
        std::fs::write(home.folder().join("appearance.json"), r#"{"theme":"light"}"#).unwrap();
        assert_eq!(home.read(crate::sys::uid()), Ok(Some(json!({"theme": "light"}))));
    }

    #[test]
    fn a_file_symlink_cannot_make_the_launcher_read_another_file() {
        let home = Home::new();
        let secret = home.path().join("unrelated.json");
        std::fs::write(&secret, r#"{"secret":"must not escape"}"#).unwrap();
        std::os::unix::fs::symlink(&secret, home.folder().join("appearance.json")).unwrap();
        assert!(home.read(crate::sys::uid()).is_err());
    }

    #[test]
    fn a_directory_symlink_cannot_escape_the_preferences_directory() {
        let home = Home::new();
        std::fs::remove_dir(home.folder()).unwrap();
        let outside = home.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("appearance.json"), "{}").unwrap();
        std::os::unix::fs::symlink(&outside, home.folder()).unwrap();
        assert!(home.read(crate::sys::uid()).is_err());
    }

    #[test]
    fn a_fifo_or_an_oversized_file_is_refused_without_blocking() {
        let home = Home::new();
        let file = home.folder().join("appearance.json");
        crate::sys::tests::mkfifo(&file);
        assert!(home.read(crate::sys::uid()).is_err());
        std::fs::remove_file(&file).unwrap();
        std::fs::write(&file, " ".repeat(65537)).unwrap();
        assert!(home.read(crate::sys::uid()).is_err());
    }

    #[test]
    fn preferences_cannot_be_a_different_owners_file_or_a_hard_link() {
        let home = Home::new();
        let file = home.folder().join("appearance.json");
        std::fs::write(&file, "{}").unwrap();
        assert!(home.read(crate::sys::uid() + 1).is_err());
        std::fs::hard_link(&file, home.path().join("second-link.json")).unwrap();
        assert!(home.read(crate::sys::uid()).is_err());
    }

    #[test]
    fn any_desktop_entry_launches_by_its_full_path() {
        let home = Home::new();
        let entry = home.path().join(".local/share/applications/mine.desktop");
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::fs::write(&entry, "[Desktop Entry]\n").unwrap();
        assert_eq!(desktop_path(entry.to_str().unwrap()), Ok(entry.clone()));
        let script = home.path().join("script.sh");
        std::fs::write(&script, "").unwrap();
        for path in [
            "mine.desktop",
            script.to_str().unwrap(),
            entry.parent().unwrap().to_str().unwrap(),
        ] {
            assert!(desktop_path(path).is_err(), "{path}");
        }
        let folder = home.path().join("x.desktop");
        std::fs::rename(entry.parent().unwrap(), &folder).unwrap();
        assert!(desktop_path(folder.to_str().unwrap()).is_err());
    }

    fn environment(person: &Account) -> std::collections::HashMap<String, String> {
        app_environment(person, "en_GB.UTF-8").into_iter().collect()
    }

    #[test]
    fn an_app_gets_the_cursor_theme_and_size() {
        let env = environment(&person(Path::new("/home/alex")));
        assert_eq!(env["XCURSOR_THEME"], "Vanilla-DMZ");
        assert_eq!(env["XCURSOR_SIZE"], "24");
    }

    #[test]
    fn an_app_learns_it_is_on_crowbaros_desktop() {
        let env = environment(&person(Path::new("/home/alex")));
        assert_eq!(env["XDG_CURRENT_DESKTOP"], "CrowbarOS");
        assert_eq!(env["CROWBAR_SESSION"], "crowbaros");
    }

    #[test]
    fn an_app_is_the_person_at_the_ordinary_wayland_socket() {
        let mut alex = person(Path::new("/home/alex"));
        alex.uid = 1003;
        let env = environment(&alex);
        assert_eq!(
            [&env["HOME"], &env["USER"], &env["LOGNAME"], &env["SHELL"]],
            ["/home/alex", "alex", "alex", "/bin/bash"]
        );
        assert_eq!(
            [&env["XDG_RUNTIME_DIR"], &env["WAYLAND_DISPLAY"]],
            ["/run/user/1003", "wayland-0"]
        );
        assert!(env["PATH"].starts_with("/home/alex/.local/bin:"));
        assert_eq!(env["LANG"], "en_GB.UTF-8");
        alex.shell = String::new();
        assert_eq!(environment(&alex)["SHELL"], "/bin/sh");
    }

    #[test]
    fn a_program_run_as_an_app_gets_the_apps_environment_alone_and_starts_in_the_persons_home() {
        let home = TempDir::new();
        let alex = person(&home.0);
        let run = |program: &str| {
            let command = [program.to_string()];
            let out = as_app(&command, alex.uid, &alex, "en_GB.UTF-8")
                .unwrap()
                .output()
                .unwrap();
            assert!(out.status.success(), "{program}: {out:?}");
            String::from_utf8(out.stdout).unwrap()
        };
        let mut got: Vec<String> = run("/usr/bin/env").lines().map(str::to_string).collect();
        let mut app: Vec<String> = app_environment(&alex, "en_GB.UTF-8")
            .into_iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        got.sort();
        app.sort();
        assert_eq!(got, app);
        assert!(got.contains(&"CROWBAR_SESSION=crowbaros".to_string()));
        assert_eq!(Path::new(run("/bin/pwd").trim_end()), home.0.canonicalize().unwrap());
    }

    #[test]
    fn a_program_run_as_an_app_is_given_its_arguments() {
        let home = TempDir::new();
        let alex = person(&home.0);
        let command = ["/bin/echo", "--state", "a b"].map(str::to_string);
        let out = as_app(&command, alex.uid, &alex, "C.UTF-8").unwrap().output().unwrap();
        assert_eq!(String::from_utf8(out.stdout).unwrap(), "--state a b\n");
    }

    #[test]
    fn only_the_person_runs_a_program_as_an_app_and_only_a_program() {
        let alex = person(Path::new("/home/alex"));
        let env = ["/usr/bin/env".to_string()];
        assert!(as_app(&env, alex.uid + 1, &alex, "C.UTF-8").is_err());
        assert!(as_app(&env, 0, &alex, "C.UTF-8").is_err());
        assert!(as_app(&[], alex.uid, &alex, "C.UTF-8").is_err());
    }

    #[test]
    fn an_app_gets_the_systems_locale() {
        let dir = TempDir::new();
        let conf = dir.0.join("locale.conf");
        std::fs::write(&conf, "# comment\nLANG=\"en_GB.UTF-8\"\n").unwrap();
        assert_eq!(system_locale(&conf), "en_GB.UTF-8");
        assert_eq!(system_locale(&dir.0.join("missing")), "C.UTF-8");
    }

    #[test]
    fn over_its_socket_the_launcher_refuses_anyone_but_the_shell() {
        let home = Home::new();
        let dir = TempDir::new();
        let path = dir.0.join("socket");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let asker = std::thread::spawn(move || {
            let mut stream = std::os::unix::net::UnixStream::connect(&path).unwrap();
            let _ = stream.write_all(b"{\"op\":\"appearance\"}\n");
            let mut reply = String::new();
            stream.read_to_string(&mut reply).ok().map(|_| reply)
        });
        let mut ran = 0;
        serve_one(
            &listener,
            crate::sys::uid() + 1,
            &person(home.path()),
            &mut |_: &mut Command| {
                ran += 1;
                Ok(())
            },
        );
        if let Some(reply) = asker.join().unwrap() {
            assert_eq!(serde_json::from_str::<serde_json::Value>(&reply).unwrap()["ok"], false);
        }
        assert_eq!(ran, 0);
    }
}
