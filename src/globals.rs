//! Globals and the registry.
//!
//! A global is advertised to a client, and can be bound by it, only if the
//! policy table offers its interface to that client's identity (ADR 0020).
//! A client that binds a global it was not offered, by guessing its number,
//! gets the same protocol error as for a number that was never used: it
//! learns nothing about what exists. Global numbers are never reused.

use std::borrow::Cow;

use fulcrum_wire::{
    DynamicNewId, InterfaceInfo,
    protocol::{self, wayland::wl_display},
};

use crate::{
    State,
    client::{ClientId, ObjectKind, ProtocolError},
    identity::Identity,
    policy,
};

/// What a global is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GlobalKind {
    /// A `wl_output`, for the output with this number.
    Output(u64),
    SecurityContextManager,
    Compositor,
    Subcompositor,
    Shm,
    XdgWmBase,
    LayerShell,
    /// A global this build advertises before it carries out its requests.
    Unimplemented,
    /// A global a test added, of an interface Fulcrum may not even know.
    Planted,
}

#[derive(Debug)]
pub(crate) struct Global {
    pub name: u32,
    pub interface: Cow<'static, str>,
    pub info: Option<&'static InterfaceInfo>,
    pub version: u32,
    pub kind: GlobalKind,
}

#[derive(Debug, Default)]
pub(crate) struct Globals {
    pub list: Vec<Global>,
    next_name: u32,
}

impl Globals {
    pub fn get(&self, name: u32) -> Option<&Global> {
        self.list.iter().find(|global| global.name == name)
    }
}

impl State {
    /// Adds a global of an interface Fulcrum knows, and tells every client
    /// the table offers it to.
    pub(crate) fn add_global(&mut self, info: &'static InterfaceInfo, version: u32, kind: GlobalKind) -> u32 {
        assert!(
            version >= 1 && version <= info.version,
            "{} has no version {version}",
            info.name
        );
        self.add_global_named(Cow::Borrowed(info.name), Some(info), version, kind)
    }

    fn add_global_named(
        &mut self,
        interface: Cow<'static, str>,
        info: Option<&'static InterfaceInfo>,
        version: u32,
        kind: GlobalKind,
    ) -> u32 {
        self.globals.next_name += 1;
        let name = self.globals.next_name;
        for client in self.clients.values_mut() {
            if policy::offers(&client.identity, &interface) {
                client.announce(name, &interface, version);
            }
        }
        self.globals.list.push(Global {
            name,
            interface,
            info,
            version,
            kind,
        });
        name
    }

    /// Removes a global, and tells every client that was offered it.
    pub(crate) fn remove_global(&mut self, name: u32) {
        let Some(index) = self.globals.list.iter().position(|global| global.name == name) else {
            return;
        };
        let global = self.globals.list.remove(index);
        for client in self.clients.values_mut() {
            if policy::offers(&client.identity, &global.interface) {
                for registry in client.registries() {
                    client.send(
                        registry,
                        fulcrum_wire::protocol::wayland::wl_registry::Event::GlobalRemove { name },
                    );
                }
            }
        }
    }

    /// Adds a global that does nothing, of any interface, for the tests of
    /// the table: with no row it must reach nobody, whatever it is.
    #[doc(hidden)]
    pub fn plant_global(&mut self, interface: &str) -> u32 {
        let info = protocol::interface(interface);
        self.add_global_named(Cow::Owned(interface.to_string()), info, 1, GlobalKind::Planted)
    }

    /// How many clients have bound a planted global.
    #[doc(hidden)]
    pub fn planted_binds(&self) -> usize {
        self.planted_binds
    }

    /// Sends a new registry every global its client is offered.
    pub(crate) fn fill_registry(&mut self, client_id: ClientId, registry: u32) {
        let Some(client) = self.clients.get_mut(&client_id) else {
            return;
        };
        for global in &self.globals.list {
            if policy::offers(&client.identity, &global.interface) {
                client.send(
                    registry,
                    fulcrum_wire::protocol::wayland::wl_registry::Event::Global {
                        name: global.name,
                        interface: global.interface.to_string(),
                        version: global.version,
                    },
                );
            }
        }
    }

    /// `wl_registry.bind`: makes the object, if the client is offered the
    /// global, names its interface, and asks for a version it has.
    pub(crate) fn bind(
        &mut self,
        client_id: ClientId,
        registry: u32,
        name: u32,
        id: DynamicNewId,
    ) -> Result<(), ProtocolError> {
        let identity: Identity = match self.clients.get(&client_id) {
            Some(client) => client.identity.clone(),
            None => return Ok(()),
        };
        let refused = || ProtocolError {
            object: registry,
            code: wl_display::error::INVALID_OBJECT,
            message: format!("invalid global {} ({name})", id.interface),
        };
        let Some(global) = self.globals.get(name) else {
            return Err(refused());
        };
        if !policy::offers(&identity, &global.interface) {
            return Err(refused());
        }
        if global.interface != id.interface {
            return Err(ProtocolError {
                object: registry,
                code: wl_display::error::INVALID_OBJECT,
                message: format!(
                    "invalid interface for global {name}: have {}, wanted {}",
                    id.interface, global.interface
                ),
            });
        }
        if id.version == 0 || id.version > global.version {
            return Err(ProtocolError {
                object: registry,
                code: wl_display::error::INVALID_OBJECT,
                message: format!(
                    "invalid version for global {} ({name}): have {}, wanted {}",
                    global.interface, global.version, id.version
                ),
            });
        }
        let (interface, info, kind) = (global.interface.clone(), global.info, global.kind);
        let object_kind = match kind {
            GlobalKind::Output(output) => ObjectKind::Output(output),
            GlobalKind::SecurityContextManager => ObjectKind::SecurityContextManager,
            GlobalKind::Compositor => ObjectKind::Compositor,
            GlobalKind::Subcompositor => ObjectKind::Subcompositor,
            GlobalKind::Shm => ObjectKind::Shm,
            GlobalKind::XdgWmBase => ObjectKind::XdgWmBase,
            GlobalKind::LayerShell => ObjectKind::LayerShell,
            GlobalKind::Unimplemented => ObjectKind::Unimplemented,
            GlobalKind::Planted => {
                self.planted_binds += 1;
                ObjectKind::Unimplemented
            }
        };
        let client = self.clients.get_mut(&client_id).expect("checked above");
        client.create(fulcrum_wire::NewId(id.id), interface, info, id.version, object_kind)?;
        match kind {
            GlobalKind::Output(output) => self.output_bound(client_id, id.id, output),
            GlobalKind::Shm => self.shm_bound(client_id, id.id),
            _ => {}
        }
        Ok(())
    }
}
