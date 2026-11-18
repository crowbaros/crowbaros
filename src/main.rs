//! `crowbar-compositor`, being rebuilt from the ground up.
//!
//! Milestone 1 of [the plan](../docs/PLAN.md) builds the Wayland core here on
//! `fulcrum-wire`. Until it can serve a client, this program refuses to
//! start, so nothing mistakes it for a compositor. CrowbarOS builds the
//! Smithay-based baseline, tag `smithay-baseline`, meanwhile.

#![forbid(unsafe_code)]

fn main() {
    eprintln!(
        "crowbar-compositor: this build is the start of a rewrite and serves no clients yet.\n\
         It speaks {} protocol interfaces on its own wire code. See docs/PLAN.md.",
        fulcrum_wire::protocol::INTERFACES.len()
    );
    std::process::exit(2);
}
