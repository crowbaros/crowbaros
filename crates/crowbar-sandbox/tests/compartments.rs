//! What a compartment gets, and the attacks a sandbox must refuse
//! (ADR 0016, "Attacks the tests must refuse", 1 to 10).
//!
//! These start real compartments, so they need root, cgroup v2, and a
//! kernel with Landlock. `cargo test` lists them as ignored.
//! `crates/README.md` says how to run them.

use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::net::TcpListener;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use std::time::Duration;

use crowbar_sandbox::{Budget, Ended, Needs, OrderError, StartError, StartOrder, Stranger, TrustedListener, Warden};

mod common;
use common::{
    Probe, ROOMY, STORAGE, USER_IDS, config, data, eventually, field, on_disk, probe_order, processes_of, program,
    read_only, read_write, root_of, scratch, stored, user_of, warden,
};

/// What the kernel says of a process in a compartment: no capability, none
/// to be had from a program it runs, and a filter on its system calls.
const NO_PRIVILEGE: &str = "ok CapInh=0 CapPrm=0 CapEff=0 CapBnd=0 CapAmb=0 NoNewPrivs=1 Seccomp=2";

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
        // The two calls are taken away before the kernel looks for the
        // process, and the process is in no namespace the attacker sees.
        assert_eq!(attacker.did(&actions[0]), "refused EPERM", "{}", actions[0]);
        assert_eq!(attacker.did(&actions[1]), "refused EPERM", "{}", actions[1]);
        assert_eq!(attacker.did(&actions[2]), "refused ENOENT", "{}", actions[2]);
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

    let (mut order, broker) = probe_order("task-1", &["fds", "say 4 second"]);
    let (mut second, compartment_end) = UnixStream::pair().unwrap();
    order.connections.push(OwnedFd::from(compartment_end));
    let mut probe = Probe::run(order, broker);

    // The sandbox's own `/dev/null` three times, and the two connections
    // in the order they were given.
    assert_eq!(
        probe.did("fds"),
        "ok 0=/dev/null 1=/dev/null 2=/dev/null 3=socket 4=socket"
    );
    assert_eq!(probe.did("say 4 second"), "ok");
    let mut said = [0; 7];
    second.read_exact(&mut said).unwrap();
    assert_eq!(&said, b"second\n");
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
    assert_eq!(probe.did("status"), NO_PRIVILEGE);
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
    assert_eq!(probe.did("status"), NO_PRIVILEGE);
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
    for (name, (major, minor)) in [("uinput", (10, 223)), ("event0", (13, 64))] {
        let path = std::ffi::CString::new(files.join(name).to_str().unwrap()).unwrap();
        let device = libc::makedev(major, minor);
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

#[test]
#[ignore = "needs root"]
fn a_fork_bomb_stops_at_the_process_budget() {
    // Attack 7, first half.
    let mut bystander = Probe::start(Vec::new(), &["hold"]);
    assert_eq!(bystander.did("hold"), "ok");

    let mut bomb = Probe::start_as("task-1", &["forks 500", "hold"], |order| {
        order.budget = Budget { processes: 8, ..ROOMY };
    });
    // The probe is one process of the eight.
    assert_eq!(bomb.did("forks 500"), "ok made=7 then=EAGAIN");
    assert_eq!(bomb.did("hold"), "ok");

    // The broker, which is this process, and the other compartment go on.
    assert!(bystander.answers());
    let ended = bomb.compartment.stop().unwrap();
    assert!(ended.processes_refused);
    assert!(!bystander.compartment.stop().unwrap().processes_refused);
}

#[test]
#[ignore = "needs root"]
fn a_program_that_takes_all_the_memory_is_stopped_at_the_memory_budget() {
    // Attack 7, second half.
    let mut bystander = Probe::start(Vec::new(), &["hold"]);
    assert_eq!(bystander.did("hold"), "ok");
    let small = |order: &mut StartOrder| {
        order.budget = Budget {
            memory_bytes: 32 << 20,
            ..ROOMY
        }
    };

    // Inside the budget, it runs.
    let mut modest = Probe::start_as("task-1", &["eat 8"], small);
    assert_eq!(modest.did("eat 8"), "ok ate=8");
    assert!(!modest.compartment.wait().unwrap().out_of_memory);

    // At the budget the whole compartment is stopped, and it says so. The
    // machine has this much memory to spare, so the budget is what stops it.
    let mut greedy = Probe::start_as("task-1", &["eat 64", "hold"], small);
    assert_eq!(greedy.line(), None);
    let ended = greedy.compartment.wait().unwrap();
    assert_eq!(
        ended,
        Ended {
            code: None,
            out_of_memory: true,
            processes_refused: false
        }
    );
    assert!(bystander.answers());
}

#[test]
#[ignore = "needs root"]
fn a_busy_program_gets_its_share_of_a_cpu_and_no_more() {
    let mut probe = Probe::start_as("task-1", &["spin 1000"], |order| {
        // A tenth of one CPU.
        order.budget = Budget {
            cpu_millicores: 100,
            ..ROOMY
        };
    });
    let cpu: u64 = field(&probe.did("spin 1000"), "cpu").parse().unwrap();
    // A second of computing got about 100 ms of CPU time. Without the
    // budget it gets the whole second.
    assert!((1..300).contains(&cpu), "{cpu} ms");
}

#[test]
#[ignore = "needs root"]
fn writes_to_a_disk_are_held_to_the_io_budget() {
    let files = on_disk("io");
    let mut probe = Probe::start_as("task-1", &["write-direct /out/big 3072"], |order| {
        order.view.push(read_write(&files, "/out"));
        // One mebibyte a second.
        order.budget = Budget {
            io_bytes_per_second: 1 << 20,
            ..ROOMY
        };
    });
    let ms: u64 = field(&probe.did("write-direct /out/big 3072"), "ms").parse().unwrap();
    // Three mebibytes take about three seconds. Without the budget they
    // take a few milliseconds.
    assert!(ms >= 1500, "{ms} ms");
    assert_eq!(fs::metadata(files.join("big")).unwrap().len(), 3 << 20);
}

#[test]
#[ignore = "needs root"]
fn what_a_compartment_leaves_behind_is_out_of_reach_of_the_next() {
    // Attack 10. The program ends and leaves files, and a process that
    // forked twice.
    let files = scratch("left");
    let left = [
        "ids",
        "write /tmp/left x",
        "write /dev/shm/left x",
        "write /out/left x",
        "daemon /out/alive",
        "exit 0",
    ];
    let mut first = Probe::start(vec![read_write(&files, "/out")], &left);
    let first_user = user_of(&mut first);
    for action in &left[1..5] {
        assert_eq!(first.did(action), "ok", "{action}");
    }
    assert_eq!(first.compartment.wait().unwrap().code, Some(0));

    // The process ended with the program, and the user owns no file.
    assert_eq!(processes_of(first_user), Vec::<u32>::new());
    for file in ["left", "alive"] {
        assert_eq!(fs::metadata(files.join(file)).unwrap().uid(), STORAGE, "{file}");
    }

    // The next compartment is another user, with places of its own.
    let next = ["ids", "ls /tmp", "ls /dev/shm", "read /out/left", "pids"];
    let mut second = Probe::start(Vec::new(), &next);
    assert_ne!(user_of(&mut second), first_user);
    assert_eq!(second.did("ls /tmp"), "ok");
    assert_eq!(second.did("ls /dev/shm"), "ok");
    assert_eq!(second.did("read /out/left"), "refused ENOENT");
    assert_eq!(second.did("pids"), "ok 1 2");
}

#[test]
#[ignore = "needs root"]
fn a_trusted_parts_socket_refuses_a_compartment_by_its_user_id() {
    // Attack 6. The socket of a trusted part that serves root alone. Its
    // directory is put in the view, and the socket is given to the storage
    // user so that the compartment sees it as its own. Both are on purpose:
    // the only check left is the one on the user ID.
    let files = scratch("trusted");
    let path = files.join("broker.sock");
    let listener = TrustedListener::bind(&path, 0).unwrap();
    stored(&path);

    // The user it serves gets in.
    let mut root = UnixStream::connect(&path).unwrap();
    let mut served = listener.accept().unwrap().expect("root is served");
    root.write_all(b"hi").unwrap();
    let mut heard = [0; 2];
    served.read_exact(&mut heard).unwrap();
    assert_eq!(&heard, b"hi");

    let actions = ["ids", "unix-talk /trusted/broker.sock"];
    let mut probe = Probe::start(vec![read_write(&files, "/trusted")], &actions);
    let uid = user_of(&mut probe);
    // The kernel says who is calling, and the listener hangs up. A listener
    // that nobody calls would wait for ever, so it waits on a thread.
    let (tell, called) = std::sync::mpsc::channel();
    std::thread::spawn(move || tell.send(listener.accept()));
    match called.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(Err(Stranger { uid: seen }))) => assert_eq!(seen, uid),
        Ok(served) => panic!("the compartment was served: {served:?}"),
        Err(_) => panic!("nobody called: {}", probe.did("unix-talk /trusted/broker.sock")),
    }
    // The compartment gets no answer. How the hang-up looks from its end
    // depends on whether it had already written.
    let happened = probe.did("unix-talk /trusted/broker.sock");
    assert!(happened.starts_with("refused "), "{happened}");
}

#[test]
#[ignore = "needs root"]
fn a_new_warden_stops_what_the_last_one_left_running() {
    let own = || config("crowbar-warden-test-restart", 1_900_200_000..1_900_201_000);
    let first = Warden::new(own()).unwrap();
    let (order, broker) = probe_order("task-1", &["ids", "hold"]);
    let mut left = Probe::run_under(&first, order, broker);
    let uid = user_of(&mut left);
    assert_eq!(left.did("hold"), "ok");

    // There is one Warden at a time.
    assert!(Warden::new(own()).is_err());

    // The first Warden goes away without stopping its compartment.
    let (compartment, mut broker) = left.into_parts();
    std::mem::forget(compartment);
    drop(first);
    assert!(!processes_of(uid).is_empty());

    let second = Warden::new(own()).unwrap();
    assert_eq!(processes_of(uid), Vec::<u32>::new());
    assert_eq!(broker.read(&mut [0; 16]).unwrap(), 0);

    // It gives no user ID the first one gave.
    let (order, broker) = probe_order("task-1", &["ids"]);
    let mut next = Probe::run_under(&second, order, broker);
    assert!(user_of(&mut next) > uid);
}

#[test]
#[ignore = "needs root"]
fn a_start_that_is_refused_leaves_nothing_behind() {
    let cgroup = |id: &str| config("crowbar-warden-test", USER_IDS).cgroup.join(id);

    // An order the Warden does not carry out.
    let (mut order, _broker) = probe_order("task-1", &[]);
    order.view.push(read_only(&scratch("covers"), "/proc"));
    let id = order.compartment.clone();
    match warden().start(order) {
        Err(StartError::Order(OrderError::Reserved(at))) => assert_eq!(at.to_str(), Some("/proc")),
        other => panic!("{:?}", other.err()),
    }
    assert!(!cgroup(&id).exists());

    // A program that is not in the view, and a view entry that is not on
    // the machine.
    type Change = fn(&mut StartOrder);
    let changes: [(&str, Change); 2] = [
        ("/nowhere", |order| order.program = "/nowhere".into()),
        ("never-made", |order| {
            order.view.push(read_only(&scratch("gone").join("never-made"), "/gone"));
        }),
    ];
    for (named, change) in changes {
        let (mut order, mut broker) = probe_order("task-1", &["hold"]);
        change(&mut order);
        let id = order.compartment.clone();
        match warden().start(order) {
            Err(StartError::Setup(step)) => assert!(step.contains(named), "{step}"),
            other => panic!("{named}: {:?}", other.err()),
        }
        assert!(!cgroup(&id).exists(), "{named}");
        // Nothing holds the compartment's end of the connection.
        assert_eq!(broker.read(&mut [0; 16]).unwrap(), 0, "{named}");
    }

    // A compartment ID that is live.
    let (order, broker) = probe_order("task-1", &["hold"]);
    let id = order.compartment.clone();
    let mut live = Probe::run(order, broker);
    assert_eq!(live.did("hold"), "ok");
    let (mut again, _broker) = probe_order("task-1", &["hold"]);
    again.compartment = id.clone();
    match warden().start(again) {
        Err(StartError::Live(refused)) => assert_eq!(refused, id),
        other => panic!("{:?}", other.err()),
    }
    assert!(live.answers());
}

#[test]
#[ignore = "needs root"]
fn a_view_entry_is_the_place_it_names_and_never_where_a_link_leads() {
    // An earlier task could write to this folder, and left links in it.
    // An order that names one of them would show another place.
    let files = scratch("links");
    fs::create_dir(files.join("photos")).unwrap();
    std::os::unix::fs::symlink("/etc", files.join("shortcut")).unwrap();
    std::os::unix::fs::symlink(files.join("photos"), files.join("album")).unwrap();
    fs::create_dir(files.join("photos/2026")).unwrap();

    for (source, named) in [("shortcut", "shortcut"), ("album/2026", "album")] {
        let (mut order, _broker) = probe_order("task-1", &["ls /seen"]);
        order.view.push(read_only(&files.join(source), "/seen"));
        match warden().start(order) {
            Err(StartError::Setup(step)) => assert!(step.contains(named), "{step}"),
            other => panic!("{source}: {:?}", other.err()),
        }
    }
    // The same place by its own name is shown.
    let mut probe = Probe::start(vec![read_only(&files.join("photos/2026"), "/seen")], &["ls /seen"]);
    assert_eq!(probe.did("ls /seen"), "ok");
}

#[test]
#[ignore = "needs root"]
fn a_program_starts_with_the_signals_every_program_expects() {
    // The Warden's own programs ignore a broken pipe. A program that
    // inherited that would write for ever to a reader that has gone.
    let mut probe = Probe::start(Vec::new(), &["signals"]);
    assert_eq!(probe.did("signals"), "ok pipe=default blocked=0");
}

#[test]
#[ignore = "needs root"]
fn a_start_is_not_taken_for_done_when_the_init_stage_says_nothing() {
    // An init stage that takes its order and ends without a word, as one
    // would that was ended for memory while it built the sandbox. This
    // stand-in reads the whole order, and then ends.
    let stand_in = scratch("silent").join("init");
    fs::write(&stand_in, "#!/bin/sh\ntimeout 0.3 cat >/dev/null\n").unwrap();
    fs::set_permissions(&stand_in, fs::Permissions::from_mode(0o755)).unwrap();
    let mut silent = config("crowbar-warden-test-silent", 1_900_300_000..1_900_301_000);
    silent.init = stand_in;
    let warden = Warden::new(silent).unwrap();
    let (order, mut broker) = probe_order("task-1", &["hold"]);
    match warden.start(order) {
        Err(StartError::Setup(_)) => {}
        other => panic!("{:?}", other.err()),
    }
    assert_eq!(broker.read(&mut [0; 16]).unwrap(), 0);
}

#[test]
#[ignore = "needs root"]
fn a_compartment_gets_no_socket_that_ignores_its_network_namespace() {
    // Attack 9 again, and the way out. The kernel has sockets that are not
    // kept apart by network namespaces: the ones virtual machines use, which
    // have one set of ports for the whole machine and reach the hypervisor,
    // and the one that reports the machine's devices. The first compartment
    // waits on such a port.
    let mut first = Probe::start(Vec::new(), &["vsock-listen 5151", "hold"]);
    assert_eq!(first.did("vsock-listen 5151"), "refused EAFNOSUPPORT");
    assert_eq!(first.did("hold"), "ok");

    let refused = [
        "vsock-connect 5151",
        "socket vsock",
        "socket netlink-uevent",
        "socket alg",
    ];
    let given = ["socket unix", "socket inet", "socket inet6", "socket netlink-route"];
    let actions: Vec<&str> = refused.iter().chain(&given).copied().collect();
    let mut second = Probe::start(Vec::new(), &actions);
    for action in refused {
        assert_eq!(second.did(action), "refused EAFNOSUPPORT", "{action}");
    }
    // The kinds a network namespace does keep apart are left to it.
    for action in given {
        assert_eq!(second.did(action), "ok", "{action}");
    }
}

#[test]
#[ignore = "needs root"]
fn nothing_mounted_in_a_sandbox_honours_a_setuid_bit_or_a_device_file() {
    let files = scratch("mounts");
    let view = vec![read_only(&files, "/read"), read_write(&files, "/written")];
    let mut probe = Probe::start(view, &["mounts"]);
    let mounts = probe.did("mounts");
    let flags = |place: &str| -> Vec<String> { field(&mounts, place).split(',').map(str::to_string).collect() };
    let has = |place: &str, flag: &str| flags(place).iter().any(|held| held == flag);

    for mount in mounts.strip_prefix("ok ").unwrap().split(' ') {
        let place = mount.split_once('=').unwrap().0;
        assert!(has(place, "nosuid"), "{mount}");
        // The sandbox's own `/dev` holds its five devices, and is fixed.
        assert!(has(place, "nodev") || place == "/dev", "{mount}");
    }
    for place in ["/", "/dev", "/read", "/probe"] {
        assert!(has(place, "ro"), "{place}: {mounts}");
    }
    for place in ["/written", "/tmp", "/dev/shm"] {
        assert!(has(place, "rw"), "{place}: {mounts}");
    }
    for place in ["/", "/proc", "/dev", "/dev/shm"] {
        assert!(has(place, "noexec"), "{place}: {mounts}");
    }
    // Every view entry shows the storage user's files as the compartment's.
    for place in ["/read", "/written", "/probe"] {
        assert!(has(place, "idmapped"), "{place}: {mounts}");
    }
}

/// Held while a test depends on this process's core file limit, which every
/// compartment the tests start is born with.
static CORE_LIMIT: Mutex<()> = Mutex::new(());

/// Sets this process's core file limit, and gives back the one it had.
fn core_limit(limit: libc::rlimit) -> libc::rlimit {
    let mut before = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: plain calls on this process's own limit.
    unsafe {
        assert_eq!(libc::getrlimit(libc::RLIMIT_CORE, &mut before), 0);
        assert_eq!(libc::setrlimit(libc::RLIMIT_CORE, &limit), 0);
    }
    before
}

#[test]
#[ignore = "needs root"]
fn a_warden_that_may_write_no_core_file_still_starts_a_compartment() {
    // Some machines start a service, or a command under `sudo`, with a core
    // file limit of 0 that nothing but root can raise.
    let _held = CORE_LIMIT.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let before = core_limit(libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    });
    let mut probe = Probe::start(Vec::new(), &["ids", "crash"]);
    core_limit(before);
    user_of(&mut probe);
    assert_eq!(probe.compartment.wait().unwrap().code, Some(128 + libc::SIGABRT));
}

#[test]
#[ignore = "needs root"]
fn a_crash_in_a_compartment_runs_no_program_of_the_machines() {
    // Many machines hand the memory of a program that crashed to a program
    // of their own, which the kernel runs as root outside every sandbox.
    // This test makes the machine one of them until it ends.
    struct Restore(String);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = fs::write(CORE_PATTERN, &self.0);
        }
    }
    const CORE_PATTERN: &str = "/proc/sys/kernel/core_pattern";
    let files = scratch("core");
    let handler = files.join("handler");
    fs::write(&handler, "#!/bin/sh\ncat >/dev/null\n: >\"$(dirname \"$0\")/ran-$1\"\n").unwrap();
    fs::set_permissions(&handler, fs::Permissions::from_mode(0o755)).unwrap();
    let _restore = Restore(fs::read_to_string(CORE_PATTERN).unwrap());
    // `%u` is the user of the program that crashed.
    fs::write(CORE_PATTERN, format!("|{} %u", handler.display())).unwrap();

    // A crash outside every compartment reaches the handler.
    let _held = CORE_LIMIT.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    core_limit(libc::rlimit {
        rlim_cur: libc::RLIM_INFINITY,
        rlim_max: libc::RLIM_INFINITY,
    });
    let crashed = std::process::Command::new(program("crowbar-sandbox-probe"))
        .arg("crash")
        .status()
        .unwrap();
    assert!(!crashed.success());
    eventually("the handler runs for a crash outside", || files.join("ran-0").exists());

    // The same crash inside one does not.
    let mut probe = Probe::start(Vec::new(), &["ids", "crash"]);
    let uid = user_of(&mut probe);
    assert_eq!(probe.compartment.wait().unwrap().code, Some(128 + libc::SIGABRT));
    std::thread::sleep(Duration::from_millis(500));
    assert!(!files.join(format!("ran-{uid}")).exists());
}
