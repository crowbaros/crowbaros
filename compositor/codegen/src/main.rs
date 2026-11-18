//! Writes `wire/src/protocol/` from `protocols/*.xml`.

fn main() {
    let root = fulcrum_codegen::repository();
    let files = match fulcrum_codegen::generate(&root.join("protocols")) {
        Ok(files) => files,
        Err(error) => {
            eprintln!("fulcrum-codegen: {error}");
            std::process::exit(1);
        }
    };
    let dir = root.join("wire/src/protocol");
    std::fs::create_dir_all(&dir).expect("the protocol directory can be made");
    for (name, contents) in files {
        let path = dir.join(&name);
        std::fs::write(&path, contents).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        println!("wrote {}", path.display());
    }
}
