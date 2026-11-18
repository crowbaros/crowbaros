//! Generates `fulcrum-wire`'s protocol modules from the XML in `protocols/`.
//!
//! The generated code is committed, so a Base build reads no XML. Run
//! `cargo run -p fulcrum-codegen` after changing a protocol file. A test fails
//! while the committed code differs from what this would write.

pub mod xml;

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

use xml::Element;

/// One protocol file, read.
#[derive(Debug)]
pub struct Protocol {
    pub name: String,
    /// The module the file becomes, from its file name.
    pub module: String,
    pub source: String,
    pub interfaces: Vec<Interface>,
}

#[derive(Debug)]
pub struct Interface {
    pub name: String,
    pub version: u32,
    pub requests: Vec<Message>,
    pub events: Vec<Message>,
    pub enums: Vec<Enum>,
}

#[derive(Debug)]
pub struct Message {
    pub name: String,
    pub since: u32,
    pub destructor: bool,
    pub summary: Option<String>,
    pub args: Vec<Arg>,
}

#[derive(Debug)]
pub struct Arg {
    pub name: String,
    pub kind: Kind,
    pub nullable: bool,
    pub interface: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Int,
    Uint,
    Fixed,
    String,
    Object,
    NewId,
    Array,
    Fd,
}

#[derive(Debug)]
pub struct Enum {
    pub name: String,
    pub entries: Vec<(String, u32)>,
}

fn number(text: &str) -> Result<u32, String> {
    let parsed = match text.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => text.parse(),
    };
    parsed.map_err(|_| format!("{text} is not a number"))
}

fn required<'a>(element: &'a Element, attribute: &str) -> Result<&'a str, String> {
    element
        .attribute(attribute)
        .ok_or_else(|| format!("<{}> has no {attribute}", element.name))
}

fn summary(element: &Element) -> Option<String> {
    let text = element.attribute("summary").map(str::to_string).or_else(|| {
        element
            .children_named("description")
            .next()?
            .attribute("summary")
            .map(str::to_string)
    })?;
    let words: Vec<&str> = text.split_whitespace().collect();
    (!words.is_empty()).then(|| words.join(" "))
}

fn message(element: &Element) -> Result<Message, String> {
    let mut args = Vec::new();
    for arg in element.children_named("arg") {
        let kind = match required(arg, "type")? {
            "int" => Kind::Int,
            "uint" => Kind::Uint,
            "fixed" => Kind::Fixed,
            "string" => Kind::String,
            "object" => Kind::Object,
            "new_id" => Kind::NewId,
            "array" => Kind::Array,
            "fd" => Kind::Fd,
            other => return Err(format!("unknown argument type {other}")),
        };
        let nullable = arg.attribute("allow-null") == Some("true");
        if nullable && !matches!(kind, Kind::String | Kind::Object) {
            return Err(format!(
                "{} allows null, which only strings and objects may",
                required(arg, "name")?
            ));
        }
        args.push(Arg {
            name: required(arg, "name")?.to_string(),
            kind,
            nullable,
            interface: arg.attribute("interface").map(str::to_string),
        });
    }
    Ok(Message {
        name: required(element, "name")?.to_string(),
        since: element.attribute("since").map(number).transpose()?.unwrap_or(1),
        destructor: element.attribute("type") == Some("destructor"),
        summary: summary(element),
        args,
    })
}

/// Reads one protocol file.
pub fn read(path: &Path) -> Result<Protocol, String> {
    let source = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let root = xml::parse(&source).map_err(|e| format!("{}: {e}", path.display()))?;
    if root.name != "protocol" {
        return Err(format!("{}: the root is not <protocol>", path.display()));
    }
    let mut interfaces = Vec::new();
    for interface in root.children_named("interface") {
        let mut enums = Vec::new();
        for item in interface.children_named("enum") {
            let mut entries = Vec::new();
            for entry in item.children_named("entry") {
                entries.push((required(entry, "name")?.to_string(), number(required(entry, "value")?)?));
            }
            enums.push(Enum {
                name: required(item, "name")?.to_string(),
                entries,
            });
        }
        interfaces.push(Interface {
            name: required(interface, "name")?.to_string(),
            version: number(required(interface, "version")?)?,
            requests: interface
                .children_named("request")
                .map(message)
                .collect::<Result<_, _>>()?,
            events: interface
                .children_named("event")
                .map(message)
                .collect::<Result<_, _>>()?,
            enums,
        });
    }
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or("a protocol file needs a name")?;
    Ok(Protocol {
        name: required(&root, "name")?.to_string(),
        module: stem.replace('-', "_"),
        source: path.file_name().unwrap().to_string_lossy().into_owned(),
        interfaces,
    })
}

const KEYWORDS: &[&str] = &[
    "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "crate", "do", "dyn", "else",
    "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl", "in", "let", "loop", "macro", "match", "mod",
    "move", "mut", "override", "priv", "pub", "ref", "return", "self", "static", "struct", "super", "trait", "true",
    "try", "type", "typeof", "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

fn field(name: &str) -> String {
    if KEYWORDS.contains(&name) {
        format!("r#{name}")
    } else {
        name.to_string()
    }
}

fn camel(name: &str) -> String {
    name.split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            let first = chars.next().unwrap().to_ascii_uppercase();
            std::iter::once(first).chain(chars).collect::<String>()
        })
        .collect()
}

fn constant(name: &str) -> String {
    let upper = name.to_ascii_uppercase();
    if upper.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{upper}")
    } else {
        upper
    }
}

fn rust_type(arg: &Arg) -> &'static str {
    match (arg.kind, arg.nullable, arg.interface.is_some()) {
        (Kind::Int, ..) => "i32",
        (Kind::Uint, ..) => "u32",
        (Kind::Fixed, ..) => "Fixed",
        (Kind::String, false, _) => "String",
        (Kind::String, true, _) => "Option<String>",
        (Kind::Object, false, _) => "ObjectId",
        (Kind::Object, true, _) => "Option<ObjectId>",
        (Kind::NewId, _, true) => "NewId",
        (Kind::NewId, _, false) => "DynamicNewId",
        (Kind::Array, ..) => "Vec<u8>",
        (Kind::Fd, ..) => "OwnedFd",
    }
}

fn read_call(arg: &Arg) -> &'static str {
    match (arg.kind, arg.nullable, arg.interface.is_some()) {
        (Kind::Int, ..) => "int",
        (Kind::Uint, ..) => "uint",
        (Kind::Fixed, ..) => "fixed",
        (Kind::String, false, _) => "string",
        (Kind::String, true, _) => "string_opt",
        (Kind::Object, false, _) => "object",
        (Kind::Object, true, _) => "object_opt",
        (Kind::NewId, _, true) => "new_id",
        (Kind::NewId, _, false) => "dynamic_new_id",
        (Kind::Array, ..) => "array",
        (Kind::Fd, ..) => "fd",
    }
}

fn write_call(arg: &Arg, value: &str) -> String {
    let call = read_call(arg);
    match (arg.kind, arg.nullable) {
        (Kind::String, false) => format!("{call}(&{value})"),
        (Kind::String, true) => format!("{call}({value}.as_deref())"),
        (Kind::Array, _) => format!("{call}(&{value})"),
        (Kind::NewId, _) if arg.interface.is_none() => format!("{call}(&{value})"),
        _ => format!("{call}({value})"),
    }
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Int => "Int",
        Kind::Uint => "Uint",
        Kind::Fixed => "Fixed",
        Kind::String => "String",
        Kind::Object => "Object",
        Kind::NewId => "NewId",
        Kind::Array => "Array",
        Kind::Fd => "Fd",
    }
}

fn emit_infos(out: &mut String, messages: &[Message]) {
    out.push_str("&[");
    for message in messages {
        write!(
            out,
            "MessageInfo {{ name: {:?}, since: {}, destructor: {}, args: &[",
            message.name, message.since, message.destructor
        )
        .unwrap();
        for arg in &message.args {
            write!(
                out,
                "ArgInfo {{ name: {:?}, kind: ArgKind::{}, nullable: {}, interface: {:?} }}, ",
                arg.name,
                kind_name(arg.kind),
                arg.nullable,
                arg.interface.as_deref()
            )
            .unwrap();
        }
        out.push_str("] }, ");
    }
    out.push(']');
}

fn emit_decode_body(out: &mut String, messages: &[Message]) {
    writeln!(out, "            let message = match opcode {{").unwrap();
    for (opcode, message) in messages.iter().enumerate() {
        if message.args.is_empty() {
            writeln!(out, "                {opcode} => Self::{},", camel(&message.name)).unwrap();
        } else {
            writeln!(out, "                {opcode} => Self::{} {{", camel(&message.name)).unwrap();
            for arg in &message.args {
                writeln!(
                    out,
                    "                    {}: args.{}()?,",
                    field(&arg.name),
                    read_call(arg)
                )
                .unwrap();
            }
            writeln!(out, "                }},").unwrap();
        }
    }
    writeln!(
        out,
        "                _ => return Err(DecodeError::UnknownOpcode(opcode)),"
    )
    .unwrap();
    writeln!(out, "            }};").unwrap();
    writeln!(out, "            args.finish()?;").unwrap();
    writeln!(out, "            Ok(message)").unwrap();
    writeln!(out, "        }}\n").unwrap();
}

fn emit_messages(out: &mut String, kind: &str, messages: &[Message]) {
    writeln!(
        out,
        "    /// Every {} of this interface, by opcode.",
        kind.to_ascii_lowercase()
    )
    .unwrap();
    writeln!(out, "    #[derive(Debug)]").unwrap();
    writeln!(out, "    pub enum {kind} {{").unwrap();
    for message in messages {
        if let Some(summary) = &message.summary {
            writeln!(out, "        /// {summary}").unwrap();
        }
        if message.args.is_empty() {
            writeln!(out, "        {},", camel(&message.name)).unwrap();
        } else {
            writeln!(out, "        {} {{", camel(&message.name)).unwrap();
            for arg in &message.args {
                writeln!(out, "            {}: {},", field(&arg.name), rust_type(arg)).unwrap();
            }
            writeln!(out, "        }},").unwrap();
        }
    }
    writeln!(out, "    }}\n").unwrap();

    writeln!(out, "    impl {kind} {{").unwrap();
    writeln!(out, "        pub fn opcode(&self) -> u16 {{").unwrap();
    if messages.is_empty() {
        writeln!(out, "            match *self {{}}").unwrap();
    } else {
        writeln!(out, "            match self {{").unwrap();
        for (opcode, message) in messages.iter().enumerate() {
            let pattern = if message.args.is_empty() { "" } else { " { .. }" };
            writeln!(
                out,
                "                Self::{}{pattern} => {opcode},",
                camel(&message.name)
            )
            .unwrap();
        }
        writeln!(out, "            }}").unwrap();
    }
    writeln!(out, "        }}\n").unwrap();

    writeln!(
        out,
        "        pub fn decode(opcode: u16, args: &mut ArgReader<'_>) -> Result<Self, DecodeError> {{"
    )
    .unwrap();
    if messages.is_empty() {
        writeln!(out, "            let _ = args;").unwrap();
        writeln!(out, "            Err(DecodeError::UnknownOpcode(opcode))").unwrap();
        writeln!(out, "        }}\n").unwrap();
    } else {
        emit_decode_body(out, messages);
    }

    writeln!(out, "        pub fn encode(self, out: &mut ArgWriter) {{").unwrap();
    if messages.is_empty() {
        writeln!(out, "            let _ = out;").unwrap();
        writeln!(out, "            match self {{}}").unwrap();
    } else if messages.iter().all(|message| message.args.is_empty()) {
        writeln!(out, "            let _ = (self, out);").unwrap();
    } else {
        writeln!(out, "            match self {{").unwrap();
        for message in messages {
            if message.args.is_empty() {
                writeln!(out, "                Self::{} => {{}}", camel(&message.name)).unwrap();
                continue;
            }
            let fields: Vec<String> = message.args.iter().map(|arg| field(&arg.name)).collect();
            writeln!(
                out,
                "                Self::{} {{ {} }} => {{",
                camel(&message.name),
                fields.join(", ")
            )
            .unwrap();
            for (arg, name) in message.args.iter().zip(&fields) {
                writeln!(out, "                    out.{};", write_call(arg, name)).unwrap();
            }
            writeln!(out, "                }}").unwrap();
        }
        writeln!(out, "            }}").unwrap();
    }
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}\n").unwrap();
}

/// The Rust module for one protocol file.
pub fn emit(protocol: &Protocol) -> String {
    let mut out = String::new();
    writeln!(
        out,
        "// Generated by fulcrum-codegen from protocols/{}. Do not edit: change the XML\n// and run `cargo run -p fulcrum-codegen`.\n",
        protocol.source
    )
    .unwrap();
    writeln!(out, "//! The `{}` protocol.\n", protocol.name).unwrap();
    writeln!(
        out,
        "#![allow(unused_imports, clippy::large_enum_variant, clippy::match_single_binding)]\n"
    )
    .unwrap();
    writeln!(
        out,
        "use crate::codec::{{ArgInfo, ArgKind, ArgReader, ArgWriter, DecodeError, DynamicNewId, Fixed, InterfaceInfo, MessageInfo, NewId, ObjectId}};"
    )
    .unwrap();
    writeln!(out, "use std::os::fd::OwnedFd;\n").unwrap();
    for interface in &protocol.interfaces {
        writeln!(out, "pub mod {} {{", interface.name).unwrap();
        writeln!(out, "    use super::*;\n").unwrap();
        writeln!(out, "    pub const NAME: &str = {:?};", interface.name).unwrap();
        writeln!(out, "    pub const VERSION: u32 = {};\n", interface.version).unwrap();
        write!(
            out,
            "    pub static INFO: InterfaceInfo = InterfaceInfo {{ name: NAME, version: VERSION, requests: "
        )
        .unwrap();
        emit_infos(&mut out, &interface.requests);
        out.push_str(", events: ");
        emit_infos(&mut out, &interface.events);
        out.push_str(" };\n\n");
        emit_messages(&mut out, "Request", &interface.requests);
        emit_messages(&mut out, "Event", &interface.events);
        for item in &interface.enums {
            writeln!(out, "    pub mod {} {{", item.name).unwrap();
            for (name, value) in &item.entries {
                writeln!(out, "        pub const {}: u32 = {value};", constant(name)).unwrap();
            }
            writeln!(out, "    }}\n").unwrap();
        }
        writeln!(out, "}}\n").unwrap();
    }
    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

/// The module that lists every protocol and every interface.
pub fn emit_index(protocols: &[Protocol]) -> String {
    let mut out = String::from(
        "// Generated by fulcrum-codegen. Do not edit: run `cargo run -p fulcrum-codegen`.\n\n//! Every protocol Fulcrum speaks, generated from `protocols/`.\n\n",
    );
    for protocol in protocols {
        writeln!(out, "pub mod {};", protocol.module).unwrap();
    }
    out.push_str("\nuse crate::codec::InterfaceInfo;\n\n/// Every interface of every protocol.\npub static INTERFACES: &[&InterfaceInfo] = &[\n");
    for protocol in protocols {
        for interface in &protocol.interfaces {
            writeln!(out, "    &{}::{}::INFO,", protocol.module, interface.name).unwrap();
        }
    }
    out.push_str("];\n\n/// The interface with this name.\npub fn interface(name: &str) -> Option<&'static InterfaceInfo> {\n    INTERFACES.iter().copied().find(|info| info.name == name)\n}\n");
    out.push_str(
        "\n/// Reads a message with its interface's typed decoder and writes it back\n/// with the typed encoder. The property tests use it to check every message\n/// of every interface. `request` picks requests, or else events.\n#[doc(hidden)]\npub fn reencode(\n    interface: &str,\n    request: bool,\n    opcode: u16,\n    args: &mut crate::codec::ArgReader<'_>,\n) -> Option<Result<crate::codec::ArgWriter, crate::codec::DecodeError>> {\n    let mut out = crate::codec::ArgWriter::default();\n    let decoded = match (interface, request) {\n",
    );
    for protocol in protocols {
        for interface in &protocol.interfaces {
            for (request, kind) in [(true, "Request"), (false, "Event")] {
                writeln!(
                    out,
                    "        ({:?}, {request}) => {}::{}::{kind}::decode(opcode, args).map(|message| message.encode(&mut out)),",
                    interface.name, protocol.module, interface.name
                )
                .unwrap();
            }
        }
    }
    out.push_str("        _ => return None,\n    };\n    Some(decoded.map(|()| out))\n}\n");
    out
}

/// Every file to write under `wire/src/protocol/`, from every `.xml` file in
/// `protocols`.
pub fn generate(protocols: &Path) -> Result<Vec<(PathBuf, String)>, String> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(protocols)
        .map_err(|e| format!("{}: {e}", protocols.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "xml"))
        .collect();
    paths.sort();
    let read: Vec<Protocol> = paths.iter().map(|path| read(path)).collect::<Result<_, _>>()?;
    let mut files: Vec<(PathBuf, String)> = read
        .iter()
        .map(|protocol| (PathBuf::from(format!("{}.rs", protocol.module)), emit(protocol)))
        .collect();
    files.push((PathBuf::from("mod.rs"), emit_index(&read)));
    Ok(files)
}

/// The repository root, from this crate's manifest directory.
pub fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_protocol_code_is_what_the_xml_generates() {
        let root = repository();
        let generated = generate(&root.join("protocols")).unwrap();
        let mut stale = Vec::new();
        for (name, contents) in &generated {
            let path = root.join("wire/src/protocol").join(name);
            if std::fs::read_to_string(&path).ok().as_deref() != Some(contents.as_str()) {
                stale.push(name.display().to_string());
            }
        }
        assert!(stale.is_empty(), "run `cargo run -p fulcrum-codegen`; stale: {stale:?}");
    }

    #[test]
    fn every_protocol_file_is_read() {
        let generated = generate(&repository().join("protocols")).unwrap();
        let names: Vec<String> = generated.iter().map(|(name, _)| name.display().to_string()).collect();
        assert!(names.contains(&"wayland.rs".to_string()));
        assert!(names.contains(&"xdg_shell.rs".to_string()));
    }

    #[test]
    fn names_become_rust() {
        assert_eq!(camel("set_buffer_transform"), "SetBufferTransform");
        assert_eq!(field("type"), "r#type");
        assert_eq!(constant("90"), "_90");
        assert_eq!(constant("argb8888"), "ARGB8888");
    }
}
