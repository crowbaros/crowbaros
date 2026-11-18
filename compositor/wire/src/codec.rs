//! The Wayland wire format: a header, then arguments, in the host's byte
//! order.
//!
//! A message starts with the object's ID, then one word holding the message's
//! size in bytes (high 16 bits, header included) and its opcode (low 16 bits).
//! Each argument takes whole 32-bit words. Strings and arrays carry their
//! length first and are padded to a word. Descriptors travel beside the
//! bytes, as `SCM_RIGHTS`, and are taken in the order the arguments name
//! them.
//!
//! Nothing here trusts the peer. Every read is bounds-checked, every string
//! must be one NUL-terminated UTF-8 string, and anything wrong is a
//! [`DecodeError`], never a panic.

use std::{collections::VecDeque, fmt, os::fd::OwnedFd};

/// The size of a message header in bytes.
pub const HEADER_SIZE: usize = 8;

/// The largest message, header included. libwayland's limit, which every
/// client already honors.
pub const MAX_MESSAGE_SIZE: usize = 4096;

/// IDs from here up are made by the server; below it, by the client.
pub const SERVER_ID_START: u32 = 0xff00_0000;

/// An object's ID on one connection. Never zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId(pub u32);

/// The ID a client chose for an object it is creating, of an interface the
/// protocol fixes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NewId(pub u32);

/// A new object whose interface the message names, as in
/// `wl_registry.bind`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DynamicNewId {
    pub interface: String,
    pub version: u32,
    pub id: u32,
}

/// A signed 24.8 fixed-point number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Fixed(pub i32);

impl Fixed {
    pub fn from_f64(value: f64) -> Self {
        Fixed((value * 256.0).round() as i32)
    }

    pub fn to_f64(self) -> f64 {
        f64::from(self.0) / 256.0
    }

    pub fn from_int(value: i32) -> Self {
        Fixed(value.saturating_mul(256))
    }
}

/// Why a message could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// A size below the header's, not a multiple of four, or above
    /// [`MAX_MESSAGE_SIZE`].
    BadSize(usize),
    /// The arguments ran past the message's end.
    Truncated,
    /// The message has bytes after its last argument.
    TrailingBytes,
    UnknownOpcode(u16),
    NullObject,
    NullNewId,
    NullString,
    /// A string whose last byte is not NUL, or with a NUL inside it.
    BadString,
    NotUtf8,
    /// The message names a descriptor that did not arrive with it.
    MissingFd,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::BadSize(size) => write!(f, "a message of {size} bytes"),
            DecodeError::Truncated => f.write_str("arguments past the message's end"),
            DecodeError::TrailingBytes => f.write_str("bytes after the last argument"),
            DecodeError::UnknownOpcode(opcode) => write!(f, "unknown opcode {opcode}"),
            DecodeError::NullObject => f.write_str("a null object where one is required"),
            DecodeError::NullNewId => f.write_str("a new object with ID 0"),
            DecodeError::NullString => f.write_str("a null string where one is required"),
            DecodeError::BadString => f.write_str("a string that is not one NUL-terminated string"),
            DecodeError::NotUtf8 => f.write_str("a string that is not UTF-8"),
            DecodeError::MissingFd => f.write_str("a descriptor that did not arrive"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// A message's header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub object: u32,
    pub opcode: u16,
    /// The whole message's size in bytes, header included.
    pub size: usize,
}

impl Header {
    /// Reads a header from the first [`HEADER_SIZE`] bytes, refusing an
    /// impossible size before any body is read.
    pub fn parse(bytes: &[u8; HEADER_SIZE]) -> Result<Self, DecodeError> {
        let object = u32::from_ne_bytes(bytes[0..4].try_into().expect("four bytes"));
        let word = u32::from_ne_bytes(bytes[4..8].try_into().expect("four bytes"));
        let size = (word >> 16) as usize;
        if size < HEADER_SIZE || !size.is_multiple_of(4) || size > MAX_MESSAGE_SIZE {
            return Err(DecodeError::BadSize(size));
        }
        Ok(Header {
            object,
            opcode: word as u16,
            size,
        })
    }

    pub fn encode(&self) -> [u8; HEADER_SIZE] {
        let mut bytes = [0; HEADER_SIZE];
        bytes[0..4].copy_from_slice(&self.object.to_ne_bytes());
        bytes[4..8].copy_from_slice(&(((self.size as u32) << 16) | u32::from(self.opcode)).to_ne_bytes());
        bytes
    }
}

/// Reads one message's arguments, in order.
pub struct ArgReader<'a> {
    body: &'a [u8],
    at: usize,
    fds: &'a mut VecDeque<OwnedFd>,
}

impl<'a> ArgReader<'a> {
    /// `body` is the message after its header. `fds` is the connection's
    /// queue of descriptors that have arrived and are not yet taken.
    pub fn new(body: &'a [u8], fds: &'a mut VecDeque<OwnedFd>) -> Self {
        ArgReader { body, at: 0, fds }
    }

    fn word(&mut self) -> Result<u32, DecodeError> {
        let bytes = self.body.get(self.at..self.at + 4).ok_or(DecodeError::Truncated)?;
        self.at += 4;
        Ok(u32::from_ne_bytes(bytes.try_into().expect("four bytes")))
    }

    fn padded(&mut self, length: usize) -> Result<&'a [u8], DecodeError> {
        let padded = length.checked_add(3).ok_or(DecodeError::Truncated)? & !3;
        let end = self.at.checked_add(padded).ok_or(DecodeError::Truncated)?;
        let bytes = self.body.get(self.at..end).ok_or(DecodeError::Truncated)?;
        self.at = end;
        Ok(&bytes[..length])
    }

    pub fn int(&mut self) -> Result<i32, DecodeError> {
        self.word().map(|word| word as i32)
    }

    pub fn uint(&mut self) -> Result<u32, DecodeError> {
        self.word()
    }

    pub fn fixed(&mut self) -> Result<Fixed, DecodeError> {
        self.int().map(Fixed)
    }

    pub fn object(&mut self) -> Result<ObjectId, DecodeError> {
        self.object_opt()?.ok_or(DecodeError::NullObject)
    }

    pub fn object_opt(&mut self) -> Result<Option<ObjectId>, DecodeError> {
        let id = self.word()?;
        Ok((id != 0).then_some(ObjectId(id)))
    }

    pub fn new_id(&mut self) -> Result<NewId, DecodeError> {
        match self.word()? {
            0 => Err(DecodeError::NullNewId),
            id => Ok(NewId(id)),
        }
    }

    pub fn dynamic_new_id(&mut self) -> Result<DynamicNewId, DecodeError> {
        Ok(DynamicNewId {
            interface: self.string()?,
            version: self.uint()?,
            id: self.new_id()?.0,
        })
    }

    pub fn string(&mut self) -> Result<String, DecodeError> {
        self.string_opt()?.ok_or(DecodeError::NullString)
    }

    pub fn string_opt(&mut self) -> Result<Option<String>, DecodeError> {
        let length = self.word()? as usize;
        if length == 0 {
            return Ok(None);
        }
        let bytes = self.padded(length)?;
        let (last, text) = bytes.split_last().expect("length is not zero");
        if *last != 0 || text.contains(&0) {
            return Err(DecodeError::BadString);
        }
        String::from_utf8(text.to_vec())
            .map(Some)
            .map_err(|_| DecodeError::NotUtf8)
    }

    pub fn array(&mut self) -> Result<Vec<u8>, DecodeError> {
        let length = self.word()? as usize;
        self.padded(length).map(<[u8]>::to_vec)
    }

    pub fn fd(&mut self) -> Result<OwnedFd, DecodeError> {
        self.fds.pop_front().ok_or(DecodeError::MissingFd)
    }

    /// Every byte of the message was read.
    pub fn finish(&self) -> Result<(), DecodeError> {
        if self.at == self.body.len() {
            Ok(())
        } else {
            Err(DecodeError::TrailingBytes)
        }
    }
}

/// A message longer than [`MAX_MESSAGE_SIZE`], which no peer would read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooLong(pub usize);

impl fmt::Display for TooLong {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "a message of {} bytes, above the limit of {MAX_MESSAGE_SIZE}",
            self.0
        )
    }
}

impl std::error::Error for TooLong {}

/// Writes one message's arguments, in order.
#[derive(Debug, Default)]
pub struct ArgWriter {
    body: Vec<u8>,
    fds: Vec<OwnedFd>,
}

impl ArgWriter {
    fn word(&mut self, word: u32) {
        self.body.extend_from_slice(&word.to_ne_bytes());
    }

    fn padded(&mut self, bytes: &[u8]) {
        self.body.extend_from_slice(bytes);
        let padding = (4 - bytes.len() % 4) % 4;
        self.body.extend(std::iter::repeat_n(0, padding));
    }

    pub fn int(&mut self, value: i32) {
        self.word(value as u32);
    }

    pub fn uint(&mut self, value: u32) {
        self.word(value);
    }

    pub fn fixed(&mut self, value: Fixed) {
        self.int(value.0);
    }

    pub fn object(&mut self, id: ObjectId) {
        self.word(id.0);
    }

    pub fn object_opt(&mut self, id: Option<ObjectId>) {
        self.word(id.map_or(0, |id| id.0));
    }

    pub fn new_id(&mut self, id: NewId) {
        self.word(id.0);
    }

    pub fn dynamic_new_id(&mut self, id: &DynamicNewId) {
        self.string(&id.interface);
        self.uint(id.version);
        self.word(id.id);
    }

    pub fn string(&mut self, text: &str) {
        debug_assert!(
            !text.contains('\0'),
            "a string the compositor sends has a NUL inside it"
        );
        self.word(text.len() as u32 + 1);
        let mut bytes = Vec::with_capacity(text.len() + 1);
        bytes.extend_from_slice(text.as_bytes());
        bytes.push(0);
        self.padded(&bytes);
    }

    pub fn string_opt(&mut self, text: Option<&str>) {
        match text {
            Some(text) => self.string(text),
            None => self.word(0),
        }
    }

    pub fn array(&mut self, bytes: &[u8]) {
        self.word(bytes.len() as u32);
        self.padded(bytes);
    }

    pub fn fd(&mut self, fd: OwnedFd) {
        self.fds.push(fd);
    }

    /// The whole message, header first, and the descriptors that go with it.
    pub fn finish(self, object: u32, opcode: u16) -> Result<(Vec<u8>, Vec<OwnedFd>), TooLong> {
        let size = HEADER_SIZE + self.body.len();
        if size > MAX_MESSAGE_SIZE {
            return Err(TooLong(size));
        }
        let mut bytes = Vec::with_capacity(size);
        bytes.extend_from_slice(&Header { object, opcode, size }.encode());
        bytes.extend_from_slice(&self.body);
        Ok((bytes, self.fds))
    }
}

/// A request or event of a generated interface, ready to be written.
pub trait Encode {
    fn opcode(&self) -> u16;
    fn encode(self, out: &mut ArgWriter);

    /// The whole message for `object`, and the descriptors that go with it.
    fn to_message(self, object: u32) -> Result<(Vec<u8>, Vec<OwnedFd>), TooLong>
    where
        Self: Sized,
    {
        let opcode = self.opcode();
        let mut out = ArgWriter::default();
        self.encode(&mut out);
        out.finish(object, opcode)
    }
}

/// The kinds of argument a message can carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgKind {
    Int,
    Uint,
    Fixed,
    String,
    Object,
    NewId,
    Array,
    Fd,
}

/// One argument of a message, as the protocol XML describes it.
#[derive(Debug)]
pub struct ArgInfo {
    pub name: &'static str,
    pub kind: ArgKind,
    pub nullable: bool,
    /// The interface of an object or new object, where the protocol fixes it.
    pub interface: Option<&'static str>,
}

/// One request or event.
#[derive(Debug)]
pub struct MessageInfo {
    pub name: &'static str,
    /// The interface version that added it.
    pub since: u32,
    /// It destroys the object it is sent on.
    pub destructor: bool,
    pub args: &'static [ArgInfo],
}

/// One interface: its requests and events, by opcode.
#[derive(Debug)]
pub struct InterfaceInfo {
    pub name: &'static str,
    pub version: u32,
    pub requests: &'static [MessageInfo],
    pub events: &'static [MessageInfo],
}

/// An argument's value, for code that reads messages by their description
/// rather than by type: logs, tests, and tools.
#[derive(Debug)]
pub enum Arg {
    Int(i32),
    Uint(u32),
    Fixed(Fixed),
    String(Option<String>),
    Object(Option<ObjectId>),
    NewId(NewId),
    DynamicNewId(DynamicNewId),
    Array(Vec<u8>),
    Fd(OwnedFd),
}

impl MessageInfo {
    /// Reads a message's arguments by this description.
    pub fn decode(&self, args: &mut ArgReader<'_>) -> Result<Vec<Arg>, DecodeError> {
        let mut values = Vec::with_capacity(self.args.len());
        for arg in self.args {
            values.push(match arg.kind {
                ArgKind::Int => Arg::Int(args.int()?),
                ArgKind::Uint => Arg::Uint(args.uint()?),
                ArgKind::Fixed => Arg::Fixed(args.fixed()?),
                ArgKind::String if arg.nullable => Arg::String(args.string_opt()?),
                ArgKind::String => Arg::String(Some(args.string()?)),
                ArgKind::Object if arg.nullable => Arg::Object(args.object_opt()?),
                ArgKind::Object => Arg::Object(Some(args.object()?)),
                ArgKind::NewId if arg.interface.is_some() => Arg::NewId(args.new_id()?),
                ArgKind::NewId => Arg::DynamicNewId(args.dynamic_new_id()?),
                ArgKind::Array => Arg::Array(args.array()?),
                ArgKind::Fd => Arg::Fd(args.fd()?),
            });
        }
        args.finish()?;
        Ok(values)
    }

    /// The object arguments of a message body, without reading its
    /// descriptors: each one's description and the ID it names (zero for
    /// none). The dispatch checks each names an object of the right
    /// interface before the message is decoded.
    pub fn object_args(&'static self, body: &[u8]) -> Result<Vec<(&'static ArgInfo, u32)>, DecodeError> {
        let mut no_fds = VecDeque::new();
        let mut reader = ArgReader::new(body, &mut no_fds);
        let mut found = Vec::new();
        for arg in self.args {
            match arg.kind {
                ArgKind::Int | ArgKind::Uint | ArgKind::Fixed => {
                    reader.uint()?;
                }
                ArgKind::NewId if arg.interface.is_some() => {
                    reader.uint()?;
                }
                ArgKind::NewId => {
                    reader.string_opt()?;
                    reader.uint()?;
                    reader.uint()?;
                }
                ArgKind::String => {
                    reader.string_opt()?;
                }
                ArgKind::Array => {
                    reader.array()?;
                }
                ArgKind::Fd => {}
                ArgKind::Object => found.push((arg, reader.uint()?)),
            }
        }
        Ok(found)
    }

    /// Writes arguments by this description. Each value must be of the kind
    /// the description gives, or the message is not one a peer would read.
    pub fn encode(&self, values: Vec<Arg>, out: &mut ArgWriter) {
        for value in values {
            match value {
                Arg::Int(value) => out.int(value),
                Arg::Uint(value) => out.uint(value),
                Arg::Fixed(value) => out.fixed(value),
                Arg::String(value) => out.string_opt(value.as_deref()),
                Arg::Object(value) => out.object_opt(value),
                Arg::NewId(value) => out.new_id(value),
                Arg::DynamicNewId(value) => out.dynamic_new_id(&value),
                Arg::Array(value) => out.array(&value),
                Arg::Fd(value) => out.fd(value),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read<T>(body: &[u8], f: impl FnOnce(&mut ArgReader<'_>) -> Result<T, DecodeError>) -> Result<T, DecodeError> {
        let mut fds = VecDeque::new();
        let mut reader = ArgReader::new(body, &mut fds);
        let value = f(&mut reader)?;
        reader.finish()?;
        Ok(value)
    }

    #[test]
    fn a_header_holds_the_object_size_and_opcode() {
        let header = Header {
            object: 3,
            opcode: 7,
            size: 20,
        };
        assert_eq!(Header::parse(&header.encode()), Ok(header));
    }

    #[test]
    fn impossible_sizes_are_refused_from_the_header_alone() {
        for size in [0usize, 4, 6, 10, MAX_MESSAGE_SIZE + 4, 0xfffc] {
            let mut bytes = [0u8; HEADER_SIZE];
            bytes[4..8].copy_from_slice(&((size as u32) << 16).to_ne_bytes());
            assert_eq!(Header::parse(&bytes), Err(DecodeError::BadSize(size)), "size {size}");
        }
    }

    #[test]
    fn strings_are_nul_terminated_padded_and_utf8() {
        let mut out = ArgWriter::default();
        out.string("abc");
        out.string_opt(None);
        out.string("wxyz");
        let (bytes, _) = out.finish(1, 0).unwrap();
        let body = &bytes[HEADER_SIZE..];
        assert_eq!(body.len(), 4 + 4 + 4 + 4 + 8);
        let (a, b, c) = read(body, |r| Ok((r.string()?, r.string_opt()?, r.string()?))).unwrap();
        assert_eq!((a.as_str(), b, c.as_str()), ("abc", None, "wxyz"));
    }

    #[test]
    fn bad_strings_are_refused() {
        let word = |n: u32| n.to_ne_bytes().to_vec();
        let cases: [(Vec<u8>, DecodeError); 5] = [
            ([word(0)].concat(), DecodeError::NullString),
            ([word(4), b"abcd".to_vec()].concat(), DecodeError::BadString),
            ([word(4), b"a\0c\0".to_vec()].concat(), DecodeError::BadString),
            ([word(3), vec![0xff, 0xfe, 0, 0]].concat(), DecodeError::NotUtf8),
            ([word(9), b"abcd".to_vec()].concat(), DecodeError::Truncated),
        ];
        for (body, error) in cases {
            assert_eq!(read(&body, |r| r.string()), Err(error));
        }
    }

    #[test]
    fn huge_lengths_do_not_overflow() {
        let body = u32::MAX.to_ne_bytes();
        assert_eq!(read(&body, |r| r.array()), Err(DecodeError::Truncated));
        assert_eq!(read(&body, |r| r.string()), Err(DecodeError::Truncated));
    }

    #[test]
    fn null_objects_and_ids_are_refused_where_required() {
        let zero = 0u32.to_ne_bytes();
        assert_eq!(read(&zero, |r| r.object()), Err(DecodeError::NullObject));
        assert_eq!(read(&zero, |r| r.object_opt()), Ok(None));
        assert_eq!(read(&zero, |r| r.new_id()), Err(DecodeError::NullNewId));
    }

    #[test]
    fn a_missing_descriptor_is_an_error() {
        assert_eq!(read(&[], |r| r.fd().map(drop)), Err(DecodeError::MissingFd));
    }

    #[test]
    fn trailing_bytes_are_refused() {
        assert_eq!(read(&[0; 8], |r| r.int()), Err(DecodeError::TrailingBytes));
    }

    #[test]
    fn an_oversized_message_is_not_written() {
        let mut out = ArgWriter::default();
        out.array(&[0; MAX_MESSAGE_SIZE]);
        assert!(out.finish(1, 0).is_err());
    }

    #[test]
    fn fixed_point_round_trips() {
        assert_eq!(Fixed::from_f64(1.5).0, 384);
        assert_eq!(Fixed(384).to_f64(), 1.5);
        assert_eq!(Fixed::from_int(-2).to_f64(), -2.0);
    }
}
