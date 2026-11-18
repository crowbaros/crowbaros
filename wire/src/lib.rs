//! Fulcrum's Wayland wire protocol: the codec, one end of a connection, and
//! the protocol code generated from `protocols/`.
//!
//! The same code serves the compositor and the test clients. It holds no
//! `unsafe` code, and nothing a peer sends can make it panic.

#![forbid(unsafe_code)]

pub mod codec;
#[rustfmt::skip]
pub mod protocol;
pub mod socket;

pub use codec::{
    Arg, ArgInfo, ArgKind, ArgReader, ArgWriter, DecodeError, DynamicNewId, Encode, Fixed, Header, InterfaceInfo,
    MessageInfo, NewId, ObjectId, TooLong,
};
pub use socket::{Connection, ConnectionError, Incoming};
