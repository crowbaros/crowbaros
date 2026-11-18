//! The kernel's device events, over netlink: a monitor plugged in or taken
//! out, and an input device added or removed.
//!
//! Fulcrum listens to the kernel's own messages rather than linking libudev.
//! Each is a line of the action and the device's path, then `KEY=VALUE`
//! pairs, each ended by a NUL.

use std::{
    collections::HashMap,
    io,
    os::fd::{AsFd, BorrowedFd, OwnedFd},
};

use rustix::net::{AddressFamily, SocketFlags, SocketType, netlink};

/// One device event.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Uevent {
    pub action: String,
    pub properties: HashMap<String, String>,
}

impl Uevent {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.properties.get(key).map(String::as_str)
    }

    /// Reads one message. A message that is not a kernel event, such as one
    /// udev rebroadcast, is ignored.
    pub fn parse(bytes: &[u8]) -> Option<Uevent> {
        let mut fields = bytes.split(|byte| *byte == 0).filter(|field| !field.is_empty());
        let first = std::str::from_utf8(fields.next()?).ok()?;
        let (action, _) = first.split_once('@')?;
        let mut event = Uevent {
            action: action.to_string(),
            properties: HashMap::new(),
        };
        for field in fields {
            let Ok(field) = std::str::from_utf8(field) else {
                continue;
            };
            if let Some((key, value)) = field.split_once('=') {
                event.properties.insert(key.to_string(), value.to_string());
            }
        }
        Some(event)
    }
}

pub struct Uevents {
    fd: OwnedFd,
}

impl Uevents {
    pub fn open() -> io::Result<Self> {
        let fd = rustix::net::socket_with(
            AddressFamily::NETLINK,
            SocketType::DGRAM,
            SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
            Some(netlink::KOBJECT_UEVENT),
        )?;
        // Group 1: the kernel's own messages.
        rustix::net::bind(&fd, &netlink::SocketAddrNetlink::new(0, 1))?;
        Ok(Uevents { fd })
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// Every event that has arrived.
    pub fn read(&self) -> Vec<Uevent> {
        let mut events = Vec::new();
        let mut buffer = vec![0u8; 8192];
        loop {
            match rustix::net::recv(&self.fd, &mut buffer, rustix::net::RecvFlags::DONTWAIT) {
                Ok((0, _)) => break,
                Ok((read, _)) => {
                    if let Some(event) = Uevent::parse(&buffer[..read.min(buffer.len())]) {
                        events.push(event);
                    }
                }
                Err(_) => break,
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kernel_event_is_read() {
        let message = b"change@/devices/pci0000:00/0000:00:01.0/drm/card0\0ACTION=change\0DEVPATH=/devices/pci0000:00/0000:00:01.0/drm/card0\0SUBSYSTEM=drm\0HOTPLUG=1\0DEVNAME=dri/card0\0";
        let event = Uevent::parse(message).unwrap();
        assert_eq!(event.action, "change");
        assert_eq!(event.get("SUBSYSTEM"), Some("drm"));
        assert_eq!(event.get("HOTPLUG"), Some("1"));
        assert_eq!(event.get("DEVNAME"), Some("dri/card0"));
    }

    #[test]
    fn a_message_that_is_not_an_event_is_ignored() {
        assert_eq!(Uevent::parse(b"libudev\0\xfe\xed\xca\xfe"), None);
        assert_eq!(Uevent::parse(b""), None);
    }
}
