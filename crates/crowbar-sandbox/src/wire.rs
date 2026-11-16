//! What the Warden tells its init stage: the order, with the user ID the
//! Warden chose. It is sent over a socket as fields that each end in a NUL
//! byte, after a message that carries its length and the compartment's
//! broker connections. No field holds a NUL: the order was checked.

use std::path::PathBuf;

use crate::order::{Access, Needs, ViewEntry};

/// What the init stage writes back when the sandbox is built, as the last
/// thing before it becomes the program. The socket then closes with
/// nothing more, or with the reason the program could not be run. An init
/// stage that ends before this has started nothing.
pub(crate) const BUILT: u8 = 0;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct InitOrder {
    /// The compartment's user and group ID.
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    /// The user and group that own Data on disk.
    pub(crate) storage_uid: u32,
    pub(crate) storage_gid: u32,
    /// An empty directory to build the sandbox's root on.
    pub(crate) root: PathBuf,
    pub(crate) needs: Needs,
    pub(crate) program: PathBuf,
    pub(crate) args: Vec<String>,
    pub(crate) env: Vec<(String, String)>,
    pub(crate) view: Vec<ViewEntry>,
}

impl InitOrder {
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut fields: Vec<String> = vec![
            self.uid.to_string(),
            self.gid.to_string(),
            self.storage_uid.to_string(),
            self.storage_gid.to_string(),
            text(&self.root),
            u8::from(self.needs.tracing).to_string(),
            u8::from(self.needs.user_namespaces).to_string(),
            text(&self.program),
            self.args.len().to_string(),
        ];
        fields.extend(self.args.iter().cloned());
        fields.push(self.env.len().to_string());
        fields.extend(self.env.iter().map(|(name, value)| format!("{name}={value}")));
        fields.push(self.view.len().to_string());
        for entry in &self.view {
            fields.push(text(&entry.source));
            fields.push(text(&entry.at));
            fields.push(u8::from(entry.access == Access::ReadWrite).to_string());
        }
        fields.iter().flat_map(|field| field.bytes().chain([0])).collect()
    }

    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        let mut fields = std::str::from_utf8(bytes).ok()?.split_terminator('\0');
        let mut next = || fields.next();
        let number = |field: Option<&str>| field?.parse::<u32>().ok();
        let flag = |field: Option<&str>| Some(number(field)? == 1);
        let order = InitOrder {
            uid: number(next())?,
            gid: number(next())?,
            storage_uid: number(next())?,
            storage_gid: number(next())?,
            root: PathBuf::from(next()?),
            needs: Needs {
                tracing: flag(next())?,
                user_namespaces: flag(next())?,
            },
            program: PathBuf::from(next()?),
            args: (0..number(next())?)
                .map(|_| next().map(str::to_string))
                .collect::<Option<_>>()?,
            env: (0..number(next())?)
                .map(|_| {
                    let (name, value) = next()?.split_once('=')?;
                    Some((name.to_string(), value.to_string()))
                })
                .collect::<Option<_>>()?,
            view: (0..number(next())?)
                .map(|_| {
                    Some(ViewEntry {
                        source: PathBuf::from(next()?),
                        at: PathBuf::from(next()?),
                        access: if flag(next())? {
                            Access::ReadWrite
                        } else {
                            Access::ReadOnly
                        },
                    })
                })
                .collect::<Option<_>>()?,
        };
        next().is_none().then_some(order)
    }
}

/// A path of a checked order, which is plain text.
fn text(path: &std::path::Path) -> String {
    path.to_str()
        .expect("a checked order holds only UTF-8 paths")
        .to_string()
}
