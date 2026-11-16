//! seccomp: system calls a compartment never makes.
//!
//! The filter takes calls away. It is a list of calls that are refused,
//! and every other call is left to the kernel's own checks. Four groups:
//!
//! - tracing and reading another process's memory, unless tracing inside
//!   the compartment is a stated need;
//! - making a user namespace and changing mounts, unless user namespaces
//!   are a stated need. A new user namespace gives a process capabilities
//!   inside it, and many kernel privilege bugs start there (ADR 0016);
//! - the kernel's keyrings, which hold credentials that belong to no
//!   compartment;
//! - calls with a large part of the kernel behind them that no Tool needs:
//!   loading kernel code, `bpf`, `perf_event_open`, `userfaultfd`,
//!   `io_uring`, and a few that only root may make in any case.
//!
//! A refused call fails with `EPERM`. `clone3` fails with `ENOSYS`,
//! because a filter cannot read its flags, and the C library then uses
//! `clone`, whose flags it can.
//!
//! Sockets are the one place where the filter lists what is allowed. A
//! network namespace keeps Unix, IPv4, and IPv6 sockets apart, and the
//! routing kind of netlink socket. It does not keep apart the sockets that
//! virtual machines use, which have one set of ports for the whole machine
//! and reach the hypervisor, nor the netlink socket that reports the
//! machine's devices. So a compartment can make the first four kinds, and
//! any other fails with `EAFNOSUPPORT`, as on a kernel built without it.
//!
//! What the filter does not do: a process can still open the memory file
//! of its own child under `/proc`, as the kernel lets a parent of the same
//! user. That crosses no compartment.

use std::io;

use libc::{c_long, sock_filter, sock_fprog};

use crate::order::Needs;
use crate::sys::checked;

#[cfg(target_arch = "x86_64")]
const ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const ARCH: u32 = 0xc000_00b7;

/// Where the fields of `struct seccomp_data` are. The machine is
/// little-endian, so the low half of an argument comes first.
const NUMBER: u32 = 0;
const ARCHITECTURE: u32 = 4;
const FIRST_ARGUMENT: u32 = 16;
const THIRD_ARGUMENT: u32 = 32;

const LOAD: u16 = (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16;
const IF_EQUAL: u16 = (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16;
#[cfg(target_arch = "x86_64")]
const IF_AT_LEAST: u16 = (libc::BPF_JMP | libc::BPF_JGE | libc::BPF_K) as u16;
const IF_ANY_BIT: u16 = (libc::BPF_JMP | libc::BPF_JSET | libc::BPF_K) as u16;
const RETURN: u16 = (libc::BPF_RET | libc::BPF_K) as u16;

const ALLOW: u32 = libc::SECCOMP_RET_ALLOW;
const KILL: u32 = libc::SECCOMP_RET_KILL_PROCESS;
const fn refuse(error: i32) -> u32 {
    libc::SECCOMP_RET_ERRNO | error as u32
}

const TRACING: [c_long; 5] = [
    libc::SYS_ptrace,
    libc::SYS_process_vm_readv,
    libc::SYS_process_vm_writev,
    libc::SYS_pidfd_getfd,
    libc::SYS_kcmp,
];

const MOUNTS: [c_long; 12] = [
    libc::SYS_mount,
    libc::SYS_umount2,
    libc::SYS_pivot_root,
    libc::SYS_chroot,
    libc::SYS_open_tree,
    libc::SYS_move_mount,
    libc::SYS_fsopen,
    libc::SYS_fsconfig,
    libc::SYS_fsmount,
    libc::SYS_fspick,
    libc::SYS_mount_setattr,
    libc::SYS_setns,
];

/// The calls that make a socket, by the family in their first argument and
/// the protocol in their third.
const SOCKETS: [c_long; 2] = [libc::SYS_socket, libc::SYS_socketpair];

/// The socket families a network namespace keeps apart.
const FAMILIES: [i32; 3] = [libc::AF_UNIX, libc::AF_INET, libc::AF_INET6];

/// The calls that make a process, by flags in their first argument.
const WITH_FLAGS: [c_long; 2] = [libc::SYS_clone, libc::SYS_unshare];

const NEVER: [c_long; 21] = [
    libc::SYS_add_key,
    libc::SYS_request_key,
    libc::SYS_keyctl,
    libc::SYS_init_module,
    libc::SYS_finit_module,
    libc::SYS_delete_module,
    libc::SYS_kexec_load,
    libc::SYS_kexec_file_load,
    libc::SYS_reboot,
    libc::SYS_swapon,
    libc::SYS_swapoff,
    libc::SYS_acct,
    libc::SYS_quotactl,
    libc::SYS_syslog,
    libc::SYS_open_by_handle_at,
    libc::SYS_bpf,
    libc::SYS_perf_event_open,
    libc::SYS_userfaultfd,
    libc::SYS_io_uring_setup,
    libc::SYS_io_uring_enter,
    libc::SYS_io_uring_register,
];

fn statement(code: u16, k: u32) -> sock_filter {
    sock_filter { code, jt: 0, jf: 0, k }
}

/// Runs the next instruction when the test holds, and skips `skip`
/// instructions when it does not.
fn unless(code: u16, k: u32, skip: u8) -> sock_filter {
    sock_filter {
        code,
        jt: 0,
        jf: skip,
        k,
    }
}

fn filter(needs: &Needs) -> Vec<sock_filter> {
    let mut program = vec![
        // A call made the way another architecture makes it is not one this
        // filter can read. Nothing here makes one.
        statement(LOAD, ARCHITECTURE),
        sock_filter {
            code: IF_EQUAL,
            jt: 1,
            jf: 0,
            k: ARCH,
        },
        statement(RETURN, KILL),
        statement(LOAD, NUMBER),
    ];
    // The x32 calls of x86-64 have numbers of their own, from this bit up.
    #[cfg(target_arch = "x86_64")]
    program.extend([
        unless(IF_AT_LEAST, 0x4000_0000, 1),
        statement(RETURN, refuse(libc::EPERM)),
    ]);

    let mut refused: Vec<c_long> = NEVER.to_vec();
    if !needs.tracing {
        refused.extend(TRACING);
    }
    if !needs.user_namespaces {
        refused.extend(MOUNTS);
    }
    for number in refused {
        program.extend([
            unless(IF_EQUAL, number as u32, 1),
            statement(RETURN, refuse(libc::EPERM)),
        ]);
    }
    if !needs.user_namespaces {
        program.extend([
            unless(IF_EQUAL, libc::SYS_clone3 as u32, 1),
            statement(RETURN, refuse(libc::ENOSYS)),
        ]);
        for number in WITH_FLAGS {
            program.extend([
                unless(IF_EQUAL, number as u32, 4),
                statement(LOAD, FIRST_ARGUMENT),
                unless(IF_ANY_BIT, libc::CLONE_NEWUSER as u32, 1),
                statement(RETURN, refuse(libc::EPERM)),
                // The next test is on the call's number again.
                statement(LOAD, NUMBER),
            ]);
        }
    }
    for number in SOCKETS {
        // The instructions after the test on the call's number, up to and
        // including the two that return.
        let rest = FAMILIES.len() as u8 + 6;
        program.push(unless(IF_EQUAL, number as u32, rest));
        program.push(statement(LOAD, FIRST_ARGUMENT));
        for (index, family) in FAMILIES.iter().enumerate() {
            // To the last instruction of this block, which allows.
            let to_allow = rest - 3 - index as u8;
            program.push(sock_filter {
                code: IF_EQUAL,
                jt: to_allow,
                jf: 0,
                k: *family as u32,
            });
        }
        program.extend([
            // Netlink, and of netlink only routing.
            unless(IF_EQUAL, libc::AF_NETLINK as u32, 2),
            statement(LOAD, THIRD_ARGUMENT),
            sock_filter {
                code: IF_EQUAL,
                jt: 1,
                jf: 0,
                k: libc::NETLINK_ROUTE as u32,
            },
            statement(RETURN, refuse(libc::EAFNOSUPPORT)),
            statement(RETURN, ALLOW),
        ]);
    }
    program.push(statement(RETURN, ALLOW));
    program
}

/// Puts the filter on this process, and on every process it starts. It
/// must already be unable to gain privileges.
pub(crate) fn confine(needs: &Needs) -> io::Result<()> {
    let mut program = filter(needs);
    let program = sock_fprog {
        len: program.len() as u16,
        filter: program.as_mut_ptr(),
    };
    // SAFETY: `program` points at the filter, which lives until the call
    // returns. The kernel copies it.
    checked(unsafe {
        libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER,
            &program as *const sock_fprog,
        )
    })?;
    Ok(())
}
