//! The Crowbar-managed block inside /etc/fstab.
//!
//! The root and tmpfs lines belong to the base image. Mounts declared in the
//! state live in one marked block so the engine can replace it wholesale.

use crowbar_state::Mount;

pub const BEGIN: &str = "# BEGIN crowbar-managed mounts (edit the Crowbar State, not this block)";
pub const END: &str = "# END crowbar-managed mounts";

pub fn render_lines(mounts: &[Mount]) -> Vec<String> {
    mounts
        .iter()
        .map(|m| format!("{}\t{}\t{}\t{}\t0 0", m.device, m.target, m.fstype, m.options))
        .collect()
}

/// The lines currently inside the managed block.
pub fn managed_lines(fstab: &str) -> Vec<String> {
    let mut inside = false;
    let mut lines = Vec::new();
    for line in fstab.lines() {
        if line == BEGIN {
            inside = true;
        } else if line == END {
            inside = false;
        } else if inside {
            lines.push(line.to_string());
        }
    }
    lines
}

/// Replace the managed block (or add one) and leave every other line alone.
pub fn splice(fstab: &str, mounts: &[Mount]) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut inside = false;
    for line in fstab.lines() {
        if line == BEGIN {
            inside = true;
            continue;
        }
        if line == END {
            inside = false;
            continue;
        }
        if !inside {
            out.push(line);
        }
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    let mut text = out.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    let lines = render_lines(mounts);
    if !lines.is_empty() {
        text.push('\n');
        text.push_str(BEGIN);
        text.push('\n');
        for line in lines {
            text.push_str(&line);
            text.push('\n');
        }
        text.push_str(END);
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mount(target: &str) -> Mount {
        Mount {
            device: "LABEL=data".into(),
            target: target.into(),
            fstype: "ext4".into(),
            options: "defaults".into(),
        }
    }

    const BASE: &str = "LABEL=crowbar-root / btrfs rw,relatime 0 0\ntmpfs /tmp tmpfs defaults 0 0\n";

    #[test]
    fn adds_replaces_and_removes_block() {
        let one = splice(BASE, &[mount("/srv/data")]);
        assert!(one.starts_with(BASE));
        assert_eq!(managed_lines(&one), vec!["LABEL=data\t/srv/data\text4\tdefaults\t0 0"]);

        let two = splice(&one, &[mount("/srv/a"), mount("/srv/b")]);
        assert_eq!(managed_lines(&two).len(), 2);
        assert_eq!(two.matches(BEGIN).count(), 1);

        let none = splice(&two, &[]);
        assert_eq!(none, BASE);
        assert!(managed_lines(&none).is_empty());
    }

    #[test]
    fn splice_is_idempotent() {
        let once = splice(BASE, &[mount("/srv/data")]);
        assert_eq!(splice(&once, &[mount("/srv/data")]), once);
    }
}
