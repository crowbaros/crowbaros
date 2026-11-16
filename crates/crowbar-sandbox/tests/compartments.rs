//! What a compartment gets, and the attacks a sandbox must refuse
//! (ADR 0016, "Attacks the tests must refuse", 1 to 10).
//!
//! These start real compartments, so they need root, cgroup v2, and a
//! kernel with Landlock. `cargo test` lists them as ignored. The crate's
//! README says how to run them.

use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::net::TcpListener;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use crowbar_sandbox::Needs;

mod common;
use common::{
    Probe, STORAGE, USER_IDS, data, eventually, field, probe_order, processes_of, program, read_only, read_write,
    root_of, scratch, stored, user_of,
};

#[test]
#[ignore = "needs root"]
fn a_compartment_sees_the_view_it_was_given_and_nothing_else() {
    let files = scratch("view");
    for directory in ["photos", "out"] {
        fs::create_dir(files.join(directory)).unwrap();
        common::stored(&files.join(directory));
    }
    data(&files.join("photos/cat.jpg"), "meow");

    let (mut order, broker) = probe_order(
        "task-1",
        &[
            "ids",
            "ls /",
            "read /photos/cat.jpg",
            "owner /photos/cat.jpg",
            "write /photos/new.jpg purr",
            "write /out/sorted.txt done",
            "owner /out/sorted.txt",
            "owner /probe",
            "write /intruder x",
            "exit 7",
        ],
    );
    order.view.push(read_only(&files.join("photos"), "/photos"));
    order.view.push(read_write(&files.join("out"), "/out"));
    let root = root_of(&order);
    let mut probe = Probe::run(order, broker);

    // It runs as a user of its own, from the Warden's range, in no group
    // but its own.
    let ids = probe.did("ids");
    let uid: u32 = field(&ids, "uid").parse().unwrap();
    assert!(USER_IDS.contains(&uid), "{ids}");
    assert_eq!(ids, format!("ok uid={uid} gid={uid} groups=[]"));

    // The root holds the view and the sandbox's own places.
    assert_eq!(probe.did("ls /"), format!("ok {root}"));

    // Data in the view is the compartment's own to read, and to change
    // where the order says so.
    assert_eq!(probe.did("read /photos/cat.jpg"), "ok meow");
    assert_eq!(probe.did("owner /photos/cat.jpg"), format!("ok {uid}"));
    assert_eq!(probe.did("write /photos/new.jpg purr"), "refused EROFS");
    assert_eq!(probe.did("write /out/sorted.txt done"), "ok");
    assert_eq!(probe.did("owner /out/sorted.txt"), format!("ok {uid}"));
    // A file that is not the storage user's is nobody's.
    assert_eq!(probe.did("owner /probe"), "ok 65534");
    // Outside the view there is nowhere to write.
    assert_eq!(probe.did("write /intruder x"), "refused EROFS");

    let ended = probe.compartment.wait().unwrap();
    assert_eq!(ended.code, Some(7));

    // What it wrote is on disk as the storage user's. The compartment's own
    // user owns nothing that outlives it.
    let written = files.join("out/sorted.txt");
    assert_eq!(fs::read_to_string(&written).unwrap(), "done");
    assert_eq!(fs::metadata(&written).unwrap().uid(), STORAGE);
    assert!(!files.join("photos/new.jpg").exists());
}

#[test]
#[ignore = "needs root"]
fn a_stop_ends_every_process_and_what_it_held_open() {
    // Attack 3, and revocation. Linux cannot take back an open descriptor,
    // so taking back a native task's authority means stopping its sandbox.
    let files = scratch("stop");
    let written = files.join("alive");
    let mut probe = Probe::start(vec![read_write(&files, "/out")], &["ids", "daemon /out/alive", "hold"]);
    let uid = user_of(&mut probe);
    assert_eq!(probe.did("daemon /out/alive"), "ok");
    assert_eq!(probe.did("hold"), "ok");

    // The probe, and the process that forked twice and started a session
    // of its own, which is writing through a file it holds open.
    assert!(processes_of(uid).len() >= 2, "{:?}", processes_of(uid));
    let size = || fs::metadata(&written).map_or(0, |file| file.len());
    let before = size();
    eventually("the file grows", || size() > before);

    let (compartment, mut broker) = probe.into_parts();
    compartment.stop().unwrap();

    // No process of that user is left, and the file has stopped growing.
    assert_eq!(processes_of(uid), Vec::<u32>::new());
    let stopped = size();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(size(), stopped);
    // Nothing holds the compartment's end of the broker connection.
    assert_eq!(broker.read(&mut [0; 16]).unwrap(), 0);
    assert_eq!(broker.write(b"ping\n").unwrap_err().kind(), ErrorKind::BrokenPipe);
}

#[test]
#[ignore = "needs root"]
fn a_compartment_sees_no_process_but_its_own() {
    let mut other = Probe::start(Vec::new(), &["hold"]);
    assert_eq!(other.did("hold"), "ok");
    let mut probe = Probe::start(Vec::new(), &["pids"]);
    // The Warden's init stage, and the probe.
    assert_eq!(probe.did("pids"), "ok 1 2");
}

#[test]
#[ignore = "needs root"]
fn a_compartment_cannot_trace_or_read_another_or_the_broker() {
    // Attack 1. The broker here is this test's own process.
    let mut victim = Probe::start(Vec::new(), &["ids", "hold"]);
    let uid = user_of(&mut victim);
    assert_eq!(victim.did("hold"), "ok");
    let mut targets = processes_of(uid);
    targets.push(std::process::id());
    assert!(targets.len() >= 2, "{targets:?}");

    for target in targets {
        let actions = [
            format!("trace {target}"),
            format!("peek {target}"),
            format!("mem {target}"),
        ];
        let mut attacker = Probe::start(Vec::new(), &actions.each_ref().map(String::as_str));
        for action in &actions {
            let happened = attacker.did(action);
            assert!(happened.starts_with("refused "), "{action}: {happened}");
        }
    }
    assert!(victim.answers());
}

#[test]
#[ignore = "needs root"]
fn a_preview_cannot_signal_the_live_compartment_of_the_same_tool() {
    // Attack 2. One principal, two compartments.
    let mut live = Probe::start_as("tool-photo-sorter", &["ids", "hold"], |_| {});
    let uid = user_of(&mut live);
    assert_eq!(live.did("hold"), "ok");

    let kill = libc::SIGKILL;
    let mut actions: Vec<String> = processes_of(uid)
        .iter()
        .map(|pid| format!("kill {pid} {kill}"))
        .collect();
    assert!(!actions.is_empty());
    // Every process it is allowed to signal, which is none outside itself.
    actions.push(format!("kill -1 {kill}"));
    let actions: Vec<&str> = actions.iter().map(String::as_str).collect();
    let mut preview = Probe::start_as("tool-photo-sorter", &actions, |_| {});
    for action in &actions[..actions.len() - 1] {
        assert_eq!(preview.did(action), "refused ESRCH", "{action}");
    }
    preview.compartment.wait().unwrap();

    assert!(live.answers());
}

#[test]
#[ignore = "needs root"]
fn two_compartments_cannot_meet() {
    // Attack 9: an abstract socket, System V shared memory, and `/tmp`.
    // The first compartment makes each meeting place and waits there.
    let mut first = Probe::start(
        Vec::new(),
        &[
            "abstract-listen crowbar-meet",
            "shm-make 49374",
            "write /tmp/meet here",
            "write /dev/shm/meet here",
            "hold",
        ],
    );
    for made in [
        "abstract-listen crowbar-meet",
        "shm-make 49374",
        "write /tmp/meet here",
        "write /dev/shm/meet here",
    ] {
        assert_eq!(first.did(made), "ok", "{made}");
    }
    assert_eq!(first.did("hold"), "ok");

    let mut second = Probe::start(
        Vec::new(),
        &[
            "abstract-connect crowbar-meet",
            "shm-find 49374",
            "read /tmp/meet",
            "read /dev/shm/meet",
            "ls /tmp",
        ],
    );
    assert_eq!(second.did("abstract-connect crowbar-meet"), "refused ECONNREFUSED");
    assert_eq!(second.did("shm-find 49374"), "refused ENOENT");
    assert_eq!(second.did("read /tmp/meet"), "refused ENOENT");
    assert_eq!(second.did("read /dev/shm/meet"), "refused ENOENT");
    assert_eq!(second.did("ls /tmp"), "ok");
    assert!(first.answers());
}

#[test]
#[ignore = "needs root"]
fn a_compartment_has_no_way_out_but_its_broker_connection() {
    // A service on this machine, listening on every address it has.
    let listener = TcpListener::bind("0.0.0.0:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let actions = [
        "interfaces".to_string(),
        format!("connect 127.0.0.1:{port}"),
        format!("send 127.0.0.1:{port}"),
        // An address on another machine.
        "connect 192.0.2.1:80".to_string(),
        "send 192.0.2.1:53".to_string(),
    ];
    let mut probe = Probe::start(Vec::new(), &actions.each_ref().map(String::as_str));

    // The loopback interface is the only one, and it is down.
    assert_eq!(probe.did("interfaces"), "ok lo");
    for action in &actions[1..] {
        let happened = probe.did(action);
        assert!(happened.starts_with("refused "), "{action}: {happened}");
    }
    probe.compartment.wait().unwrap();
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
}

#[test]
#[ignore = "needs root"]
fn a_compartment_starts_with_its_broker_connections_and_no_other_descriptor() {
    // Attack 5. A careless trusted part: descriptors that are open in the
    // Warden's process and are not closed when it runs a program.
    // SAFETY: plain calls that open descriptors for the rest of the run.
    unsafe {
        assert!(libc::open(c"/etc/hostname".as_ptr(), libc::O_RDONLY) > 0);
        assert!(libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) > 0);
        assert_eq!(libc::pipe([0; 2].as_mut_ptr()), 0);
    }

    let (mut order, broker) = probe_order("task-1", &["fds", "say 4 proxied"]);
    let (mut proxy, compartment_end) = UnixStream::pair().unwrap();
    order.connections.push(OwnedFd::from(compartment_end));
    let mut probe = Probe::run(order, broker);

    // The sandbox's own `/dev/null` three times, and the two connections
    // in the order they were given.
    assert_eq!(
        probe.did("fds"),
        "ok 0=/dev/null 1=/dev/null 2=/dev/null 3=socket 4=socket"
    );
    assert_eq!(probe.did("say 4 proxied"), "ok");
    let mut said = [0; 8];
    proxy.read_exact(&mut said).unwrap();
    assert_eq!(&said, b"proxied\n");
}

#[test]
#[ignore = "needs root"]
fn a_compartment_has_none_of_the_credentials_around_it() {
    // This process has an environment, root's groups, every capability,
    // and a session keyring. The compartment has what its order says.
    assert!(std::env::var_os("PATH").is_some());
    let mut probe = Probe::start_as("task-1", &["env", "status", "keyring"], |order| {
        order.env = vec![("LANG".to_string(), "C.UTF-8".to_string())];
    });
    assert_eq!(probe.did("env"), "ok LANG=C.UTF-8");
    // No capability, none to be had from a program it runs, and a filter
    // on its system calls.
    assert_eq!(
        probe.did("status"),
        "ok CapInh=0 CapPrm=0 CapEff=0 CapBnd=0 CapAmb=0 NoNewPrivs=1 Seccomp=2"
    );
    assert_eq!(probe.did("keyring"), "refused EPERM");
}

#[test]
#[ignore = "needs root"]
fn a_setuid_program_in_the_view_gains_nothing() {
    // Attack 4: a copy of the probe that root owns, with the setuid and
    // setgid bits.
    let files = scratch("setuid");
    let planted = files.join("escalate");
    fs::copy(program("crowbar-sandbox-probe"), &planted).unwrap();
    fs::set_permissions(&planted, fs::Permissions::from_mode(0o6755)).unwrap();

    let actions = ["ids", "exec /planted/escalate ids status"];
    let mut probe = Probe::start(vec![read_only(&files, "/planted")], &actions);
    let before = probe.did("ids");
    assert_eq!(probe.did("ids"), before);
    assert_eq!(
        probe.did("status"),
        "ok CapInh=0 CapPrm=0 CapEff=0 CapBnd=0 CapAmb=0 NoNewPrivs=1 Seccomp=2"
    );
}

#[test]
#[ignore = "needs root"]
fn a_compartment_cannot_make_a_user_namespace_a_mount_or_a_network_interface() {
    // Attack 8.
    let actions = ["userns", "mount /tmp", "interface"];
    let mut probe = Probe::start(Vec::new(), &actions);
    for action in actions {
        assert_eq!(probe.did(action), "refused EPERM", "{action}");
    }
}

#[test]
#[ignore = "needs root"]
fn a_compartment_has_no_input_device_and_can_neither_make_nor_open_one() {
    // The device numbers of `/dev/uinput` and of the first input device.
    // Both are planted in the view on purpose, where anyone may open them.
    let files = scratch("devices");
    for (name, minor_of) in [("uinput", (10, 223)), ("event0", (13, 64))] {
        let path = std::ffi::CString::new(files.join(name).to_str().unwrap()).unwrap();
        let device = libc::makedev(minor_of.0, minor_of.1);
        // SAFETY: `path` is a C string.
        assert_eq!(unsafe { libc::mknod(path.as_ptr(), libc::S_IFCHR | 0o666, device) }, 0);
        fs::set_permissions(files.join(name), fs::Permissions::from_mode(0o666)).unwrap();
        stored(&files.join(name));
    }

    let actions = [
        "ls /dev",
        "open /dev/uinput",
        "open /dev/input/event0",
        "mknod /tmp/uinput 10 223",
        "mknod /planted/own 10 223",
        "open /planted/uinput",
        "open /planted/event0",
        "open /dev/null",
    ];
    let mut probe = Probe::start(vec![read_write(&files, "/planted")], &actions);
    assert_eq!(
        probe.did("ls /dev"),
        "ok fd full null random shm stderr stdin stdout urandom zero"
    );
    assert_eq!(probe.did("open /dev/uinput"), "refused ENOENT");
    assert_eq!(probe.did("open /dev/input/event0"), "refused ENOENT");
    assert_eq!(probe.did("mknod /tmp/uinput 10 223"), "refused EPERM");
    assert_eq!(probe.did("mknod /planted/own 10 223"), "refused EPERM");
    assert_eq!(probe.did("open /planted/uinput"), "refused EACCES");
    assert_eq!(probe.did("open /planted/event0"), "refused EACCES");
    assert_eq!(probe.did("open /dev/null"), "ok");
}

#[test]
#[ignore = "needs root"]
fn tracing_is_refused_even_inside_a_compartment_unless_it_is_a_stated_need() {
    let mut plain = Probe::start(Vec::new(), &["trace-child"]);
    assert_eq!(plain.did("trace-child"), "refused EPERM");

    // With the need, a debugger works on the compartment's own processes
    // and on nothing else.
    let broker = format!("trace {}", std::process::id());
    let mut debugger = Probe::start_as("task-1", &["trace-child", &broker], |order| {
        order.needs = Needs {
            tracing: true,
            ..Needs::default()
        };
    });
    assert_eq!(debugger.did("trace-child"), "ok");
    assert_eq!(debugger.did(&broker), "refused ESRCH");
}

#[test]
#[ignore = "needs root"]
fn a_user_namespace_as_a_stated_need_still_changes_no_view() {
    // Inside a user namespace of its own a process has every capability
    // over the mounts it sees. The second filesystem check still holds.
    let actions = ["userns", "mount /tmp", "write /intruder x"];
    let mut probe = Probe::start_as("task-1", &actions, |order| {
        order.needs = Needs {
            user_namespaces: true,
            ..Needs::default()
        };
    });
    assert_eq!(probe.did("userns"), "ok");
    assert_eq!(probe.did("mount /tmp"), "refused EPERM");
    assert_eq!(probe.did("write /intruder x"), "refused EROFS");
}

#[test]
#[ignore = "needs root"]
fn a_compartment_cannot_open_a_port_even_inside_its_own_network() {
    // Nothing could reach the port. The second check refuses it anyway, so
    // a mistake in the network setup does not become a listening service.
    let mut probe = Probe::start(Vec::new(), &["listen"]);
    assert_eq!(probe.did("listen"), "refused EACCES");
}
