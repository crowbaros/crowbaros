//! Crowbar State: the one declared document that describes a CrowbarOS system.
//!
//! Milestone 1 covers a deliberately small surface: hostname, packages, runit
//! services, whole managed files, and mounts. The agent edits this document;
//! the apply engine is the only thing that turns it into a running system.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};
use std::fmt;

/// The only schema version Milestone 1 understands.
pub const SCHEMA_VERSION: u32 = 1;

/// Paths the state document may never manage as plain files. Each is either
/// owned by another state section or by the apply engine itself.
const PROTECTED_PREFIXES: &[&str] = &["/proc", "/sys", "/dev", "/run", "/etc/crowbar", "/boot"];
const PROTECTED_FILES: &[&str] = &["/etc/fstab", "/etc/hostname"];

/// The apply engine writes a file as `.<name><TEMP_SUFFIX>` next to its final
/// name and renames it into place. A `[[file]]` may not use such a name, or
/// writing its neighbour would remove it.
pub const TEMP_SUFFIX: &str = ".crowbar-tmp";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub version: u32,
    pub system: System,
    #[serde(default)]
    pub packages: Packages,
    #[serde(default)]
    pub services: Services,
    #[serde(default, rename = "file", skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<ManagedFile>,
    #[serde(default, rename = "mount", skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<Mount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct System {
    pub hostname: String,
}

/// Packages the state owns. Packages present in the base image but not listed
/// here are left alone; packages removed from this list are uninstalled.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Packages {
    #[serde(default)]
    pub install: BTreeSet<String>,
}

/// The exact set of runit services linked into the default runlevel.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Services {
    #[serde(default)]
    pub enabled: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedFile {
    pub path: String,
    pub content: String,
    #[serde(default = "default_mode")]
    pub mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mount {
    pub device: String,
    pub target: String,
    pub fstype: String,
    #[serde(default = "default_mount_options")]
    pub options: String,
}

fn default_mode() -> String {
    "0644".to_string()
}

fn default_mount_options() -> String {
    "defaults".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateError {
    Parse(String),
    Invalid(Vec<String>),
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StateError::Parse(msg) => write!(f, "state document does not parse: {msg}"),
            StateError::Invalid(problems) => {
                write!(f, "state document is invalid:")?;
                for problem in problems {
                    write!(f, "\n  - {problem}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for StateError {}

impl ManagedFile {
    /// The file mode as a number, for example `0o644`.
    pub fn mode_bits(&self) -> Result<u32, String> {
        u32::from_str_radix(&self.mode, 8)
            .ok()
            .filter(|bits| *bits <= 0o7777)
            .ok_or_else(|| {
                format!(
                    "file {}: mode {:?} is not an octal mode like \"0644\"",
                    self.path, self.mode
                )
            })
    }
}

impl State {
    /// Parse and validate a state document.
    pub fn from_toml_str(text: &str) -> Result<State, StateError> {
        let state: State = toml::from_str(text).map_err(|e| StateError::Parse(e.to_string()))?;
        state.validate()?;
        Ok(state)
    }

    pub fn to_toml_string(&self) -> String {
        toml::to_string(self).expect("state always serializes")
    }

    /// A stable fingerprint of the canonical form of this state (FNV-1a, 64-bit).
    /// It identifies a state in the ledger; it is not a security boundary.
    pub fn fingerprint(&self) -> String {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in self.to_toml_string().bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        format!("{hash:016x}")
    }

    pub fn validate(&self) -> Result<(), StateError> {
        let mut problems = Vec::new();
        if self.version != SCHEMA_VERSION {
            problems.push(format!(
                "version {} is not supported (expected {SCHEMA_VERSION})",
                self.version
            ));
        }
        if !valid_hostname(&self.system.hostname) {
            problems.push(format!(
                "system.hostname {:?} must be 1-63 lowercase letters, digits, or '-', not starting or ending with '-'",
                self.system.hostname
            ));
        }
        for name in &self.packages.install {
            if !valid_name(name, "._+-") {
                problems.push(format!("package name {name:?} is not a valid xbps package name"));
            }
        }
        for name in &self.services.enabled {
            if !valid_name(name, "._@-") {
                problems.push(format!("service name {name:?} is not a valid runit service name"));
            }
        }
        let mut paths = HashSet::new();
        for file in &self.files {
            if let Err(problem) = check_managed_path(&file.path) {
                problems.push(problem);
            }
            if !paths.insert(file.path.as_str()) {
                problems.push(format!("file {} is declared more than once", file.path));
            }
            if let Err(problem) = file.mode_bits() {
                problems.push(problem);
            }
        }
        let mut targets = HashSet::new();
        for mount in &self.mounts {
            if !is_clean_absolute(&mount.target) || mount.target == "/" {
                problems.push(format!(
                    "mount target {:?} must be an absolute path other than /",
                    mount.target
                ));
            }
            if !fstab_safe(&mount.target) {
                problems.push(format!(
                    "mount target {:?} must not contain whitespace, control characters, or backslashes",
                    mount.target
                ));
            }
            if in_protected_location(&mount.target) {
                problems.push(format!("mount target {} is in a protected location", mount.target));
            }
            if mount.device.starts_with('#') {
                problems.push(format!(
                    "mount {:?}: device {:?} starts with '#', which fstab reads as a comment",
                    mount.target, mount.device
                ));
            }
            if !targets.insert(mount.target.as_str()) {
                problems.push(format!("mount target {} is declared more than once", mount.target));
            }
            for (field, value) in [
                ("device", &mount.device),
                ("fstype", &mount.fstype),
                ("options", &mount.options),
            ] {
                if value.is_empty() || !fstab_safe(value) {
                    problems.push(format!(
                        "mount {:?}: {field} must be non-empty with no whitespace, control characters, or backslashes",
                        mount.target
                    ));
                }
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(StateError::Invalid(problems))
        }
    }
}

fn valid_hostname(name: &str) -> bool {
    (1..=63).contains(&name.len())
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn valid_name(name: &str, extra: &str) -> bool {
    !name.is_empty()
        && name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

/// An fstab field is one whitespace-separated word, and fstab reads `\040`
/// style escapes in it, so none of these characters can be written as-is.
fn fstab_safe(value: &str) -> bool {
    !value.chars().any(|c| c.is_whitespace() || c.is_control() || c == '\\')
}

/// Absolute, no empty, `.` or `..` components, no trailing slash.
pub fn is_clean_absolute(path: &str) -> bool {
    path.starts_with('/')
        && (path == "/" || !path.ends_with('/'))
        && path[1..]
            .split('/')
            .all(|part| path == "/" || !(part.is_empty() || part == "." || part == ".."))
}

/// Whether `path` may be managed as a `[[file]]`. The apply engine calls this
/// again on the path a `[[file]]` resolves to inside a generation, so a
/// symlink cannot be used to reach a protected location.
pub fn check_managed_path(path: &str) -> Result<(), String> {
    if !is_clean_absolute(path) || path == "/" {
        return Err(format!("file path {path:?} must be a clean absolute path"));
    }
    // The path is printed in the plan and written to the ledger, one line each.
    if path
        .chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
    {
        return Err(format!(
            "file path {path:?} must not contain control characters or line breaks"
        ));
    }
    if PROTECTED_FILES.contains(&path) {
        return Err(format!(
            "file {path} is managed by another state section and cannot be a [[file]]"
        ));
    }
    if in_protected_location(path) {
        return Err(format!("file {path} is in a protected location"));
    }
    let name = path.rsplit('/').next().unwrap_or_default();
    if name.starts_with('.') && name.ends_with(TEMP_SUFFIX) {
        return Err(format!(
            "file {path} has the name the apply engine gives its temporary files (.<name>{TEMP_SUFFIX})"
        ));
    }
    Ok(())
}

/// At or under a location that belongs to the kernel, the boot loader, or the
/// apply engine.
fn in_protected_location(path: &str) -> bool {
    PROTECTED_PREFIXES
        .iter()
        .any(|p| path == *p || path.starts_with(&format!("{p}/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_managed_path_cannot_hold_a_line_break_or_control_character() {
        for bad in [
            "/etc/x\n  ~ write file /etc/motd (mode 0644)",
            "/etc/x\ry",
            "/etc/x\ty",
            "/etc/x\u{1b}[2Ky",
            "/etc/x\u{85}y",
            "/etc/x\u{2028}y",
        ] {
            let problem = check_managed_path(bad).unwrap_err();
            assert!(problem.contains("control characters"), "{bad:?}: {problem}");
        }
        // Spaces and letters outside ASCII are ordinary file names.
        assert!(check_managed_path("/etc/my notes/café.conf").is_ok());
    }

    const EXAMPLE: &str = r#"
version = 1

[system]
hostname = "crowbar-lab"

[packages]
install = ["jq", "htop"]

[services]
enabled = ["sshd", "dhcpcd", "agetty-ttyS0"]

[[file]]
path = "/etc/motd"
content = "hello\n"

[[mount]]
device = "LABEL=crowbar-data"
target = "/srv/data"
fstype = "ext4"
"#;

    fn invalid(text: &str) -> Vec<String> {
        match State::from_toml_str(text) {
            Err(StateError::Invalid(problems)) => problems,
            other => panic!("expected validation failure, got {other:?}"),
        }
    }

    #[test]
    fn parses_example_with_defaults() {
        let state = State::from_toml_str(EXAMPLE).unwrap();
        assert_eq!(state.system.hostname, "crowbar-lab");
        assert!(state.packages.install.contains("jq"));
        assert_eq!(state.files[0].mode, "0644");
        assert_eq!(state.files[0].mode_bits().unwrap(), 0o644);
        assert_eq!(state.mounts[0].options, "defaults");
    }

    #[test]
    fn roundtrips_and_fingerprint_is_stable() {
        let state = State::from_toml_str(EXAMPLE).unwrap();
        let again = State::from_toml_str(&state.to_toml_string()).unwrap();
        assert_eq!(state, again);
        assert_eq!(state.fingerprint(), again.fingerprint());
        let mut changed = state.clone();
        changed.packages.install.insert("tmux".into());
        assert_ne!(state.fingerprint(), changed.fingerprint());
    }

    #[test]
    fn rejects_unknown_fields() {
        let err = State::from_toml_str("version = 1\nsurprise = true\n[system]\nhostname = \"a\"\n").unwrap_err();
        assert!(matches!(err, StateError::Parse(_)));
    }

    #[test]
    fn rejects_wrong_version_and_bad_hostname() {
        let problems = invalid("version = 2\n[system]\nhostname = \"Bad_Host\"\n");
        assert_eq!(problems.len(), 2, "{problems:?}");
    }

    #[test]
    fn rejects_protected_relative_and_duplicate_files() {
        let problems = invalid(
            r#"version = 1
[system]
hostname = "a"
[[file]]
path = "/etc/fstab"
content = ""
[[file]]
path = "etc/motd"
content = ""
[[file]]
path = "/etc/crowbar/state.toml"
content = ""
[[file]]
path = "/etc/../shadow"
content = ""
[[file]]
path = "/etc/issue"
content = ""
mode = "999"
[[file]]
path = "/etc/issue"
content = ""
"#,
        );
        assert_eq!(problems.len(), 6, "{problems:?}");
    }

    #[test]
    fn rejects_bad_mounts_and_names() {
        let problems = invalid(
            r#"version = 1
[system]
hostname = "a"
[packages]
install = ["-rf"]
[services]
enabled = ["../etc"]
[[mount]]
device = "LABEL=x"
target = "/"
fstype = "ext4"
[[mount]]
device = "LABEL=y"
target = "/srv"
fstype = "ext 4"
"#,
        );
        assert_eq!(problems.len(), 4, "{problems:?}");
    }

    #[test]
    fn rejects_mount_fields_that_would_break_fstab() {
        for target in [
            "/srv/my data",
            "/srv/data\\n/dev/sda1 / ext4 defaults 0 0",
            "/srv/a\\\\040b",
        ] {
            let text = format!(
                "version = 1\n[system]\nhostname = \"a\"\n[[mount]]\ndevice = \"LABEL=x\"\ntarget = \"{target}\"\nfstype = \"ext4\"\n"
            );
            let problems = invalid(&text);
            assert_eq!(problems.len(), 1, "{target:?}: {problems:?}");
            assert!(problems[0].contains("mount target"), "{problems:?}");
        }
        let problems = invalid(
            "version = 1\n[system]\nhostname = \"a\"\n[[mount]]\ndevice = \"LABEL=x\\u001b\"\ntarget = \"/srv\"\nfstype = \"ext4\"\n",
        );
        assert_eq!(problems.len(), 1, "{problems:?}");
    }

    fn mount_problems(device: &str, target: &str) -> Vec<String> {
        let text = format!(
            "version = 1\n[system]\nhostname = \"a\"\n[[mount]]\ndevice = \"{device}\"\ntarget = \"{target}\"\nfstype = \"ext4\"\n"
        );
        match State::from_toml_str(&text) {
            Ok(_) => Vec::new(),
            Err(StateError::Invalid(problems)) => problems,
            Err(other) => panic!("{other:?}"),
        }
    }

    #[test]
    fn rejects_a_mount_device_that_fstab_reads_as_a_comment() {
        let problems = mount_problems("#silentlyignored", "/srv/data");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("comment"), "{problems:?}");
        assert!(mount_problems("LABEL=a#b", "/srv/data").is_empty());
    }

    #[test]
    fn rejects_mount_targets_in_protected_locations() {
        for target in ["/etc/crowbar", "/boot", "/proc", "/sys/fs", "/dev/shm", "/run/data"] {
            let problems = mount_problems("LABEL=x", target);
            assert_eq!(problems.len(), 1, "{target}: {problems:?}");
            assert!(problems[0].contains("protected location"), "{problems:?}");
        }
        assert!(mount_problems("LABEL=x", "/srv/boot").is_empty());
        assert!(mount_problems("LABEL=x", "/bootstrap").is_empty());
    }

    #[test]
    fn rejects_a_file_named_like_an_engine_temp_file() {
        let problems = invalid(
            "version = 1\n[system]\nhostname = \"a\"\n[[file]]\npath = \"/etc/.motd.crowbar-tmp\"\ncontent = \"\"\n",
        );
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("temporary files"), "{problems:?}");
        // Only the engine's own pattern is refused.
        for path in ["/etc/motd.crowbar-tmp", "/etc/.motd"] {
            let text =
                format!("version = 1\n[system]\nhostname = \"a\"\n[[file]]\npath = \"{path}\"\ncontent = \"\"\n");
            assert!(State::from_toml_str(&text).is_ok(), "{path}");
        }
    }

    #[test]
    fn clean_absolute_paths() {
        assert!(is_clean_absolute("/"));
        assert!(is_clean_absolute("/etc/motd"));
        assert!(!is_clean_absolute("/etc/"));
        assert!(!is_clean_absolute("/etc//motd"));
        assert!(!is_clean_absolute("/etc/./motd"));
        assert!(!is_clean_absolute("relative"));
    }
}
