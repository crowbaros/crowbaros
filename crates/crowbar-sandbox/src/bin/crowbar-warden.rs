//! The Warden's program.
//!
//! So far it has one use: `crowbar-warden init`, the init stage that the
//! Warden runs to build one sandbox. A person never runs it.

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("init") => crowbar_sandbox::init_stage(),
        _ => {
            eprintln!("crowbar-warden is run by the Warden, not by hand");
            std::process::exit(2);
        }
    }
}
