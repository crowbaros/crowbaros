//! The order to start one compartment, and the rules that refuse a bad one.

use std::fmt;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

/// What the broker orders the Warden to start: one program, in one new
/// compartment, with a view, a budget, and its broker connections.
///
/// The compartment gets what is written here and nothing else.
#[derive(Debug)]
pub struct StartOrder {
    /// The principal the compartment belongs to. It is stable, and authority
    /// binds to it.
    pub principal: String,
    /// The name of this one start. The broker never mints it twice.
    pub compartment: String,
    /// The program, by its path inside the view.
    pub program: PathBuf,
    /// The program's arguments, after its own name.
    pub args: Vec<String>,
    /// The program's whole environment.
    pub env: Vec<(String, String)>,
    /// What the compartment can see of the machine's files.
    pub view: Vec<ViewEntry>,
    pub budget: Budget,
    pub needs: Needs,
    /// The compartment's ends of its broker connections. The program finds
    /// them as descriptors 3, 4, and so on, in this order.
    pub connections: Vec<OwnedFd>,
}

/// One file or directory of the machine, shown inside the sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewEntry {
    /// Where it is on the machine.
    pub source: PathBuf,
    /// Where the compartment sees it.
    pub at: PathBuf,
    pub access: Access,
}

/// What a compartment may do with a view entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Read it, and run a program in it.
    ReadOnly,
    /// Change it too.
    ReadWrite,
}

/// The most a compartment may use. Every part is set, and none is zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Memory, with no swap on top. A compartment that reaches it is stopped
    /// whole.
    pub memory_bytes: u64,
    /// How many processes and threads the program may have at once.
    pub processes: u64,
    /// CPU time, in thousandths of one CPU.
    pub cpu_millicores: u64,
    /// Bytes read, and bytes written, in one second on each disk.
    pub io_bytes_per_second: u64,
}

/// What a compartment gets beyond the default, because the principal's
/// stated needs include it. The person sees each need (ADR 0016).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Needs {
    /// Tracing among the compartment's own processes, for a debugger.
    pub tracing: bool,
    /// Making user namespaces, for a program that sandboxes its own children.
    pub user_namespaces: bool,
}

/// Why the Warden refuses an order before it creates anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderError {
    /// A principal or compartment ID that is not 1 to 64 of `A-Z a-z 0-9 . _ -`,
    /// or that starts with a dot.
    Id(String),
    /// A path that is not absolute, that holds `.` or `..`, or that is not
    /// plain UTF-8 text.
    Path(PathBuf),
    /// A view entry at `/`, or at or under the sandbox's own `/proc`, `/dev`,
    /// or `/tmp`.
    Reserved(PathBuf),
    /// Two view entries, one at or inside the other.
    Overlap(PathBuf, PathBuf),
    /// An argument or environment value with a NUL byte, or an environment
    /// name that is empty or holds `=`.
    Text(String),
    /// A part of the budget that is zero.
    Budget(&'static str),
    /// More than 32 broker connections, which is what one start carries.
    Connections(usize),
}

impl StartOrder {
    /// Whether the Warden would carry this order out, as far as the order
    /// alone can say.
    pub fn check(&self) -> Result<(), OrderError> {
        for id in [&self.principal, &self.compartment] {
            if !is_id(id) {
                return Err(OrderError::Id(id.clone()));
            }
        }
        if self.connections.len() > crate::sys::MAX_PASSED {
            return Err(OrderError::Connections(self.connections.len()));
        }
        exact(&self.program)?;
        for arg in &self.args {
            plain(arg)?;
        }
        for (name, value) in &self.env {
            if name.is_empty() || name.contains('=') {
                return Err(OrderError::Text(name.clone()));
            }
            plain(name)?;
            plain(value)?;
        }
        for (index, entry) in self.view.iter().enumerate() {
            exact(&entry.source)?;
            exact(&entry.at)?;
            let own = OWN_PLACES.iter().any(|place| entry.at.starts_with(place));
            if own || entry.at == Path::new("/") {
                return Err(OrderError::Reserved(entry.at.clone()));
            }
            for earlier in &self.view[..index] {
                if entry.at.starts_with(&earlier.at) {
                    return Err(OrderError::Overlap(earlier.at.clone(), entry.at.clone()));
                }
                if earlier.at.starts_with(&entry.at) {
                    return Err(OrderError::Overlap(entry.at.clone(), earlier.at.clone()));
                }
            }
        }
        let Budget {
            memory_bytes,
            processes,
            cpu_millicores,
            io_bytes_per_second,
        } = self.budget;
        for (part, amount) in [
            ("memory", memory_bytes),
            ("processes", processes),
            ("CPU", cpu_millicores),
            ("I/O", io_bytes_per_second),
        ] {
            if amount == 0 {
                return Err(OrderError::Budget(part));
            }
        }
        Ok(())
    }
}

/// The places every sandbox provides for itself. A view entry goes beside
/// them, never over them.
pub(crate) const OWN_PLACES: [&str; 3] = ["/proc", "/dev", "/tmp"];

fn is_id(id: &str) -> bool {
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
    (1..=64).contains(&id.len()) && !id.starts_with('.') && id.chars().all(plain)
}

/// A path that names one place and can be read only one way: absolute, with
/// no `.`, `..`, or empty part, and plain text.
fn exact(path: &Path) -> Result<(), OrderError> {
    let exact = path.to_str().is_some_and(|text| {
        text == "/"
            || (text.starts_with('/')
                && !text.contains('\0')
                && text[1..].split('/').all(|part| !matches!(part, "" | "." | "..")))
    });
    if exact {
        Ok(())
    } else {
        Err(OrderError::Path(path.to_path_buf()))
    }
}

fn plain(text: &str) -> Result<(), OrderError> {
    if text.contains('\0') {
        return Err(OrderError::Text(text.to_string()));
    }
    Ok(())
}

impl fmt::Display for OrderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OrderError::Id(id) => write!(f, "{id:?} is not a usable ID"),
            OrderError::Path(path) => write!(f, "{path:?} is not an absolute, exact path"),
            OrderError::Reserved(path) => write!(f, "{path:?} is a place the sandbox itself provides"),
            OrderError::Overlap(outer, inner) => {
                write!(f, "the view entry at {inner:?} is inside the one at {outer:?}")
            }
            OrderError::Text(text) => write!(f, "{text:?} cannot be an argument or an environment entry"),
            OrderError::Budget(part) => write!(f, "the budget sets no {part}"),
            OrderError::Connections(count) => write!(f, "{count} broker connections are more than one start carries"),
        }
    }
}

impl std::error::Error for OrderError {}
