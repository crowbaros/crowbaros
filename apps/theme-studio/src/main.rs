// SPDX-License-Identifier: MIT
#[cfg(feature = "ui")]
mod ui;
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "--version") {
        println!("crowbaros-theme-studio {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if !args.is_empty() {
        let result =
            crowbar_theme::ThemeStore::for_user().and_then(|store| crowbaros_theme_studio::command(&args, &store));
        match result {
            Ok(text) => println!("{text}"),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }
    #[cfg(feature = "ui")]
    ui::run();
    #[cfg(not(feature = "ui"))]
    eprintln!("This build supports headless commands only.");
}
