//! Property tests over every message of every protocol Fulcrum speaks.
//!
//! - A valid message read by its typed decoder and written back by its typed
//!   encoder is the same bytes, with the same number of descriptors.
//! - Random bodies, every truncation of a valid message, and random byte
//!   streams never panic. A truncated message with arguments is refused.
//!
//! The cases come from a fixed seed, so a failure repeats.

use std::{collections::VecDeque, os::fd::OwnedFd, os::unix::net::UnixStream};

use fulcrum_wire::{
    Arg, ArgKind, ArgReader, ArgWriter, Connection, DynamicNewId, Fixed, Header, MessageInfo, NewId, ObjectId,
    codec::{HEADER_SIZE, MAX_MESSAGE_SIZE},
    protocol::{INTERFACES, reencode},
};

/// xorshift64*: small, fast, and the same everywhere.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    fn chance(&mut self, one_in: u64) -> bool {
        self.below(one_in) == 0
    }

    fn text(&mut self) -> String {
        const PIECES: &[&str] = &["a", "Z", "0", "_", " ", ".", "é", "ß", "漢", "🦀", "/", "-"];
        (0..self.below(24))
            .map(|_| PIECES[self.below(PIECES.len() as u64) as usize])
            .collect()
    }

    fn id(&mut self) -> u32 {
        1 + self.below(u64::from(u32::MAX - 1)) as u32
    }
}

fn fd() -> OwnedFd {
    rustix::pipe::pipe().expect("a pipe").0
}

fn random_args(info: &MessageInfo, random: &mut Random) -> Vec<Arg> {
    info.args
        .iter()
        .map(|arg| match arg.kind {
            ArgKind::Int => Arg::Int(random.next() as i32),
            ArgKind::Uint => Arg::Uint(random.next() as u32),
            ArgKind::Fixed => Arg::Fixed(Fixed(random.next() as i32)),
            ArgKind::String if arg.nullable && random.chance(3) => Arg::String(None),
            ArgKind::String => Arg::String(Some(random.text())),
            ArgKind::Object if arg.nullable && random.chance(3) => Arg::Object(None),
            ArgKind::Object => Arg::Object(Some(ObjectId(random.id()))),
            ArgKind::NewId if arg.interface.is_some() => Arg::NewId(NewId(random.id())),
            ArgKind::NewId => Arg::DynamicNewId(DynamicNewId {
                interface: random.text().replace(' ', "_") + "x",
                version: 1 + random.below(10) as u32,
                id: random.id(),
            }),
            ArgKind::Array => Arg::Array((0..random.below(64)).map(|_| random.next() as u8).collect()),
            ArgKind::Fd => Arg::Fd(fd()),
        })
        .collect()
}

/// Every (interface, request?, opcode, message) Fulcrum knows.
fn every_message() -> Vec<(&'static str, bool, u16, &'static MessageInfo)> {
    let mut all = Vec::new();
    for interface in INTERFACES {
        for (request, messages) in [(true, interface.requests), (false, interface.events)] {
            for (opcode, message) in messages.iter().enumerate() {
                all.push((interface.name, request, opcode as u16, message));
            }
        }
    }
    all
}

#[test]
fn every_message_is_known() {
    let all = every_message();
    assert!(all.len() > 200, "only {} messages", all.len());
    for (name, request, opcode, _) in &all {
        let mut fds = VecDeque::new();
        let mut reader = ArgReader::new(&[], &mut fds);
        assert!(
            reencode(name, *request, *opcode, &mut reader).is_some(),
            "{name} has no decoder"
        );
    }
}

#[test]
fn valid_messages_round_trip_through_the_typed_code() {
    let mut random = Random(0x0f1c_4a3e_9d2b_7701);
    for (name, request, opcode, info) in every_message() {
        for _ in 0..40 {
            let mut out = ArgWriter::default();
            info.encode(random_args(info, &mut random), &mut out);
            let (bytes, fds) = out.finish(7, opcode).expect("random messages are small");
            let sent_fds = fds.len();
            let mut queue: VecDeque<OwnedFd> = fds.into();
            let mut reader = ArgReader::new(&bytes[HEADER_SIZE..], &mut queue);
            let again = reencode(name, request, opcode, &mut reader)
                .expect("a decoder")
                .unwrap_or_else(|error| panic!("{name}.{}: a valid message was refused: {error}", info.name));
            assert!(queue.is_empty(), "{name}.{}: a descriptor was not taken", info.name);
            let (rebuilt, refds) = again.finish(7, opcode).unwrap();
            assert_eq!(rebuilt, bytes, "{name}.{} did not round-trip", info.name);
            assert_eq!(refds.len(), sent_fds);
        }
    }
}

#[test]
fn random_bodies_never_panic() {
    let mut random = Random(0x5eed_cafe_f00d_0001);
    for (name, request, opcode, _) in every_message() {
        for _ in 0..60 {
            let length = (random.below(48) * 4) as usize;
            let body: Vec<u8> = (0..length).map(|_| random.next() as u8).collect();
            let mut fds: VecDeque<OwnedFd> = (0..random.below(3)).map(|_| fd()).collect();
            let mut reader = ArgReader::new(&body, &mut fds);
            let _ = reencode(name, request, opcode, &mut reader);
        }
        let mut fds = VecDeque::new();
        let mut reader = ArgReader::new(&[], &mut fds);
        assert!(
            reencode(name, request, u16::MAX, &mut reader)
                .expect("a decoder")
                .is_err(),
            "{name}: an unknown opcode was accepted"
        );
    }
}

#[test]
fn every_truncation_of_a_message_with_arguments_is_refused() {
    let mut random = Random(0xdead_beef_1234_5678);
    for (name, request, opcode, info) in every_message() {
        if info.args.is_empty() {
            continue;
        }
        let mut out = ArgWriter::default();
        info.encode(random_args(info, &mut random), &mut out);
        let (bytes, fds) = out.finish(7, opcode).unwrap();
        let body = &bytes[HEADER_SIZE..];
        let has_fd = info.args.iter().any(|arg| arg.kind == ArgKind::Fd);
        for cut in (0..body.len()).step_by(4) {
            let mut queue: VecDeque<OwnedFd> = if has_fd {
                fds.iter().map(|fd| fd.try_clone().unwrap()).collect()
            } else {
                VecDeque::new()
            };
            let mut reader = ArgReader::new(&body[..cut], &mut queue);
            let result = reencode(name, request, opcode, &mut reader).expect("a decoder");
            assert!(result.is_err(), "{name}.{} was accepted cut at {cut} bytes", info.name);
        }
    }
}

#[test]
fn random_byte_streams_never_panic_the_connection() {
    let mut random = Random(0x0bad_5eed_0000_0042);
    for _ in 0..200 {
        let (a, b) = UnixStream::pair().unwrap();
        let mut sender = Connection::new(a).unwrap();
        let mut receiver = Connection::new(b).unwrap();
        let mut bytes = Vec::new();
        // Often a plausible header, so the body paths are reached too.
        for _ in 0..1 + random.below(6) {
            if random.chance(2) {
                let size = HEADER_SIZE + (random.below(16) * 4) as usize;
                let header = Header {
                    object: random.id(),
                    opcode: random.below(8) as u16,
                    size,
                };
                bytes.extend_from_slice(&header.encode());
                bytes.extend((0..size - HEADER_SIZE).map(|_| random.next() as u8));
            } else {
                bytes.extend((0..random.below(64)).map(|_| random.next() as u8));
            }
        }
        bytes.truncate(MAX_MESSAGE_SIZE);
        sender.queue(bytes.clone(), Vec::new()).unwrap();
        sender.flush().unwrap();
        let mut received = 0;
        while received < bytes.len() {
            received += receiver.fill().unwrap();
        }
        while let Ok(Some(incoming)) = receiver.next_message() {
            for interface in INTERFACES {
                let opcode = incoming.header.opcode;
                let mut fds = VecDeque::new();
                let mut reader = ArgReader::new(&incoming.body, &mut fds);
                let _ = reencode(interface.name, true, opcode, &mut reader);
            }
        }
    }
}
