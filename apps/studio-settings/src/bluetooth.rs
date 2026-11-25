// SPDX-License-Identifier: MIT
//! Bluetooth adapter and device management via BlueZ D-Bus (`org.bluez`).

use std::collections::HashMap;
use zbus::blocking::{Connection, Proxy};
use zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

const BLUEZ_DEST: &str = "org.bluez";
const BLUEZ_AGENT_PATH: &str = "/com/crowbaros/StudioSettings/BluezAgent";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BluetoothAdapter {
    pub path: String,
    pub name: String,
    pub alias: String,
    pub address: String,
    pub powered: bool,
    pub discovering: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BluetoothDevice {
    pub path: String,
    pub adapter_path: String,
    pub address: String,
    pub name: String,
    pub icon: Option<String>,
    pub paired: bool,
    pub trusted: bool,
    pub connected: bool,
    pub rssi: Option<i16>,
    pub battery_percentage: Option<u8>,
}

impl BluetoothDevice {
    pub fn status_label(&self) -> &'static str {
        if self.connected {
            "Connected"
        } else if self.paired {
            "Paired"
        } else {
            "Available"
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BluetoothState {
    Available {
        adapter: BluetoothAdapter,
        devices: Vec<BluetoothDevice>,
    },
    Unavailable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BluetoothError {
    Failed(String),
}

impl BluetoothError {
    pub fn message(&self) -> &str {
        match self {
            Self::Failed(msg) => msg,
        }
    }
}

pub type ManagedObjects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

fn get_string(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    let value = props.get(key)?;
    <&str>::try_from(value).ok().map(str::to_string)
}

fn get_bool(props: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    let value = props.get(key)?;
    bool::try_from(value).ok()
}

fn get_i16(props: &HashMap<String, OwnedValue>, key: &str) -> Option<i16> {
    let value = props.get(key)?;
    i16::try_from(value).ok()
}

fn get_u8(props: &HashMap<String, OwnedValue>, key: &str) -> Option<u8> {
    let value = props.get(key)?;
    u8::try_from(value).ok()
}

fn get_object_path(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    let value = props.get(key)?;
    match &**value {
        Value::ObjectPath(p) => Some(p.as_str().to_string()),
        _ => None,
    }
}

pub fn parse_bluez_objects(objects: &ManagedObjects) -> BluetoothState {
    let mut adapters: Vec<(&str, &HashMap<String, OwnedValue>)> = objects
        .iter()
        .filter_map(|(path, ifaces)| {
            ifaces
                .get("org.bluez.Adapter1")
                .map(|props| (path.as_str(), props))
        })
        .collect();
    adapters.sort_by_key(|(path, _)| *path);

    let Some((adapter_path, adapter_props)) = adapters.into_iter().next() else {
        return BluetoothState::Unavailable(
            "No Bluetooth adapter detected on this machine.".into(),
        );
    };

    let name = get_string(adapter_props, "Name").unwrap_or_else(|| "Bluetooth".into());
    let alias = get_string(adapter_props, "Alias").unwrap_or_else(|| name.clone());
    let address = get_string(adapter_props, "Address").unwrap_or_default();
    let powered = get_bool(adapter_props, "Powered").unwrap_or(false);
    let discovering = get_bool(adapter_props, "Discovering").unwrap_or(false);

    let adapter = BluetoothAdapter {
        path: adapter_path.to_string(),
        name,
        alias,
        address,
        powered,
        discovering,
    };

    let mut devices = Vec::new();
    for (path, ifaces) in objects {
        let Some(dev_props) = ifaces.get("org.bluez.Device1") else {
            continue;
        };
        let dev_adapter = get_object_path(dev_props, "Adapter").unwrap_or_default();
        if dev_adapter != adapter_path {
            continue;
        }
        let dev_address = get_string(dev_props, "Address").unwrap_or_default();
        let dev_name = get_string(dev_props, "Alias")
            .or_else(|| get_string(dev_props, "Name"))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| dev_address.clone());
        let battery_percentage = ifaces
            .get("org.bluez.Battery1")
            .and_then(|b| get_u8(b, "Percentage"));

        devices.push(BluetoothDevice {
            path: path.as_str().to_string(),
            adapter_path: dev_adapter,
            address: dev_address,
            name: dev_name,
            icon: get_string(dev_props, "Icon"),
            paired: get_bool(dev_props, "Paired").unwrap_or(false),
            trusted: get_bool(dev_props, "Trusted").unwrap_or(false),
            connected: get_bool(dev_props, "Connected").unwrap_or(false),
            rssi: get_i16(dev_props, "RSSI"),
            battery_percentage,
        });
    }

    // Connected first, then paired, then strongest RSSI, then alphabetical name.
    devices.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then_with(|| b.paired.cmp(&a.paired))
            .then_with(|| b.rssi.cmp(&a.rssi))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    BluetoothState::Available { adapter, devices }
}

pub fn map_bluez_error(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("serviceunknown") || lower.contains("namehasnoowner") {
        "Bluetooth service (bluetoothd) is not running.".into()
    } else if lower.contains("org.bluez.error.notready") {
        "Turn on Bluetooth before scanning or connecting.".into()
    } else if lower.contains("org.bluez.error.inprogress") {
        "A Bluetooth operation is already in progress.".into()
    } else if lower.contains("org.bluez.error.alreadyexists") {
        "This device is already paired.".into()
    } else if lower.contains("org.bluez.error.authenticationfailed")
        || lower.contains("org.bluez.error.authenticationrejected")
    {
        "Bluetooth pairing was rejected or timed out.".into()
    } else if lower.contains("org.bluez.error.failed") {
        "Could not reach this Bluetooth device. Make sure it is powered on and nearby.".into()
    } else if lower.contains("accessdenied") || lower.contains("permission") {
        "Permission denied by BlueZ. Your account must belong to the bluetooth group.".into()
    } else {
        "Bluetooth request failed. Try refreshing the device list.".into()
    }
}

fn is_service_missing(err: &zbus::Error) -> bool {
    let text = err.to_string();
    text.contains("ServiceUnknown")
        || text.contains("NameHasNoOwner")
        || text.contains("No such file or directory")
}

pub fn fetch_bluetooth() -> BluetoothState {
    let Ok(conn) = Connection::system() else {
        return BluetoothState::Unavailable("System D-Bus is unavailable on this machine.".into());
    };
    let Ok(proxy) = Proxy::new(&conn, BLUEZ_DEST, "/", "org.freedesktop.DBus.ObjectManager") else {
        return BluetoothState::Unavailable("Bluetooth service (bluetoothd) is not running.".into());
    };
    match proxy.call::<_, _, ManagedObjects>("GetManagedObjects", &()) {
        Ok(objects) => parse_bluez_objects(&objects),
        Err(err) if is_service_missing(&err) => {
            BluetoothState::Unavailable("Bluetooth service (bluetoothd) is not running.".into())
        }
        Err(err) => BluetoothState::Unavailable(map_bluez_error(&err.to_string())),
    }
}

pub fn set_powered(adapter_path: &str, powered: bool) -> Result<(), BluetoothError> {
    let conn = Connection::system()
        .map_err(|e| BluetoothError::Failed(map_bluez_error(&e.to_string())))?;
    let proxy = Proxy::new(
        &conn,
        BLUEZ_DEST,
        adapter_path,
        "org.freedesktop.DBus.Properties",
    )
    .map_err(|e| BluetoothError::Failed(map_bluez_error(&e.to_string())))?;
    proxy
        .call::<_, _, ()>("Set", &("org.bluez.Adapter1", "Powered", Value::from(powered)))
        .map_err(|e| BluetoothError::Failed(map_bluez_error(&e.to_string())))
}

pub fn set_discovery(adapter_path: &str, discover: bool) -> Result<(), BluetoothError> {
    let conn = Connection::system()
        .map_err(|e| BluetoothError::Failed(map_bluez_error(&e.to_string())))?;
    let proxy = Proxy::new(&conn, BLUEZ_DEST, adapter_path, "org.bluez.Adapter1")
        .map_err(|e| BluetoothError::Failed(map_bluez_error(&e.to_string())))?;
    let method = if discover {
        "StartDiscovery"
    } else {
        "StopDiscovery"
    };
    match proxy.call::<_, _, ()>(method, &()) {
        Ok(()) => Ok(()),
        Err(err) if err.to_string().contains("InProgress") => Ok(()),
        Err(err) => Err(BluetoothError::Failed(map_bluez_error(&err.to_string()))),
    }
}

struct BluezAgent;

#[zbus::interface(name = "org.bluez.Agent1")]
impl BluezAgent {
    fn release(&self) {}

    fn request_confirmation(
        &self,
        _device: ObjectPath<'_>,
        _passkey: u32,
    ) -> zbus::fdo::Result<()> {
        Ok(())
    }

    fn request_authorization(&self, _device: ObjectPath<'_>) -> zbus::fdo::Result<()> {
        Ok(())
    }

    fn authorize_service(&self, _device: ObjectPath<'_>, _uuid: String) -> zbus::fdo::Result<()> {
        Ok(())
    }

    fn cancel(&self) {}
}

pub fn pair_device(device_path: &str) -> Result<(), BluetoothError> {
    let conn = Connection::system()
        .map_err(|e| BluetoothError::Failed(map_bluez_error(&e.to_string())))?;

    let mut agent_registered = false;
    if conn.object_server().at(BLUEZ_AGENT_PATH, BluezAgent).is_ok()
        && let Ok(mgr) = Proxy::new(&conn, BLUEZ_DEST, "/org/bluez", "org.bluez.AgentManager1")
        && let Ok(path) = ObjectPath::try_from(BLUEZ_AGENT_PATH)
        && mgr
            .call::<_, _, ()>("RegisterAgent", &(path, "NoInputNoOutput"))
            .is_ok()
    {
        agent_registered = true;
    }

    let dev_proxy = Proxy::new(&conn, BLUEZ_DEST, device_path, "org.bluez.Device1")
        .map_err(|e| BluetoothError::Failed(map_bluez_error(&e.to_string())))?;
    let pair_result = dev_proxy.call::<_, _, ()>("Pair", &());

    if pair_result.is_ok() {
        if let Ok(props) = Proxy::new(
            &conn,
            BLUEZ_DEST,
            device_path,
            "org.freedesktop.DBus.Properties",
        ) {
            let _ = props.call::<_, _, ()>(
                "Set",
                &("org.bluez.Device1", "Trusted", Value::from(true)),
            );
        }
        let _ = dev_proxy.call::<_, _, ()>("Connect", &());
    }

    if agent_registered {
        if let Ok(mgr) = Proxy::new(&conn, BLUEZ_DEST, "/org/bluez", "org.bluez.AgentManager1")
            && let Ok(path) = ObjectPath::try_from(BLUEZ_AGENT_PATH)
        {
            let _ = mgr.call::<_, _, ()>("UnregisterAgent", &(path,));
        }
        let _ = conn.object_server().remove::<BluezAgent, _>(BLUEZ_AGENT_PATH);
    }

    pair_result.map_err(|e| BluetoothError::Failed(map_bluez_error(&e.to_string())))
}

pub fn connect_device(device_path: &str) -> Result<(), BluetoothError> {
    let conn = Connection::system()
        .map_err(|e| BluetoothError::Failed(map_bluez_error(&e.to_string())))?;
    let proxy = Proxy::new(&conn, BLUEZ_DEST, device_path, "org.bluez.Device1")
        .map_err(|e| BluetoothError::Failed(map_bluez_error(&e.to_string())))?;
    proxy
        .call::<_, _, ()>("Connect", &())
        .map_err(|e| BluetoothError::Failed(map_bluez_error(&e.to_string())))
}

pub fn disconnect_device(device_path: &str) -> Result<(), BluetoothError> {
    let conn = Connection::system()
        .map_err(|e| BluetoothError::Failed(map_bluez_error(&e.to_string())))?;
    let proxy = Proxy::new(&conn, BLUEZ_DEST, device_path, "org.bluez.Device1")
        .map_err(|e| BluetoothError::Failed(map_bluez_error(&e.to_string())))?;
    proxy
        .call::<_, _, ()>("Disconnect", &())
        .map_err(|e| BluetoothError::Failed(map_bluez_error(&e.to_string())))
}

pub fn forget_device(adapter_path: &str, device_path: &str) -> Result<(), BluetoothError> {
    let conn = Connection::system()
        .map_err(|e| BluetoothError::Failed(map_bluez_error(&e.to_string())))?;
    let proxy = Proxy::new(&conn, BLUEZ_DEST, adapter_path, "org.bluez.Adapter1")
        .map_err(|e| BluetoothError::Failed(map_bluez_error(&e.to_string())))?;
    let path = ObjectPath::try_from(device_path)
        .map_err(|e| BluetoothError::Failed(map_bluez_error(&e.to_string())))?;
    proxy
        .call::<_, _, ()>("RemoveDevice", &(path,))
        .map_err(|e| BluetoothError::Failed(map_bluez_error(&e.to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned_str(s: &str) -> OwnedValue {
        Value::from(s).try_to_owned().unwrap()
    }

    fn owned_bool(b: bool) -> OwnedValue {
        Value::from(b).try_to_owned().unwrap()
    }

    fn owned_i16(n: i16) -> OwnedValue {
        Value::from(n).try_to_owned().unwrap()
    }

    fn owned_u8(n: u8) -> OwnedValue {
        Value::from(n).try_to_owned().unwrap()
    }

    fn owned_path(p: &str) -> OwnedValue {
        Value::from(ObjectPath::try_from(p).unwrap())
            .try_to_owned()
            .unwrap()
    }

    #[test]
    fn parses_adapter_and_sorts_connected_paired_nearby_devices() {
        let mut objects: ManagedObjects = HashMap::new();
        let adapter_path = OwnedObjectPath::try_from("/org/bluez/hci0").unwrap();
        let dev_nearby = OwnedObjectPath::try_from("/org/bluez/hci0/dev_AA").unwrap();
        let dev_paired = OwnedObjectPath::try_from("/org/bluez/hci0/dev_BB").unwrap();
        let dev_connected = OwnedObjectPath::try_from("/org/bluez/hci0/dev_CC").unwrap();

        objects.insert(
            adapter_path.clone(),
            HashMap::from([(
                "org.bluez.Adapter1".into(),
                HashMap::from([
                    ("Name".into(), owned_str("crowbaros")),
                    ("Alias".into(), owned_str("CrowbarOS")),
                    ("Address".into(), owned_str("10:20:30:40:50:60")),
                    ("Powered".into(), owned_bool(true)),
                    ("Discovering".into(), owned_bool(false)),
                ]),
            )]),
        );
        objects.insert(
            dev_nearby,
            HashMap::from([(
                "org.bluez.Device1".into(),
                HashMap::from([
                    ("Adapter".into(), owned_path("/org/bluez/hci0")),
                    ("Address".into(), owned_str("AA:AA:AA:AA:AA:AA")),
                    ("Alias".into(), owned_str("Nearby Mouse")),
                    ("Paired".into(), owned_bool(false)),
                    ("Connected".into(), owned_bool(false)),
                    ("RSSI".into(), owned_i16(-62)),
                ]),
            )]),
        );
        objects.insert(
            dev_paired,
            HashMap::from([(
                "org.bluez.Device1".into(),
                HashMap::from([
                    ("Adapter".into(), owned_path("/org/bluez/hci0")),
                    ("Address".into(), owned_str("BB:BB:BB:BB:BB:BB")),
                    ("Alias".into(), owned_str("Keychron K3")),
                    ("Paired".into(), owned_bool(true)),
                    ("Trusted".into(), owned_bool(true)),
                    ("Connected".into(), owned_bool(false)),
                ]),
            )]),
        );
        objects.insert(
            dev_connected,
            HashMap::from([
                (
                    "org.bluez.Device1".into(),
                    HashMap::from([
                        ("Adapter".into(), owned_path("/org/bluez/hci0")),
                        ("Address".into(), owned_str("CC:CC:CC:CC:CC:CC")),
                        ("Alias".into(), owned_str("Studio Headphones")),
                        ("Paired".into(), owned_bool(true)),
                        ("Trusted".into(), owned_bool(true)),
                        ("Connected".into(), owned_bool(true)),
                    ]),
                ),
                (
                    "org.bluez.Battery1".into(),
                    HashMap::from([("Percentage".into(), owned_u8(85))]),
                ),
            ]),
        );

        let BluetoothState::Available { adapter, devices } = parse_bluez_objects(&objects) else {
            panic!("expected available adapter");
        };
        assert_eq!(adapter.alias, "CrowbarOS");
        assert!(adapter.powered);
        assert_eq!(devices.len(), 3);
        assert_eq!(devices[0].name, "Studio Headphones");
        assert_eq!(devices[0].battery_percentage, Some(85));
        assert_eq!(devices[0].status_label(), "Connected");
        assert_eq!(devices[1].name, "Keychron K3");
        assert_eq!(devices[1].status_label(), "Paired");
        assert_eq!(devices[2].name, "Nearby Mouse");
        assert_eq!(devices[2].status_label(), "Available");
    }

    #[test]
    fn no_adapter_in_bluez_tree_yields_calm_unavailable_state() {
        let mut objects: ManagedObjects = HashMap::new();
        objects.insert(
            OwnedObjectPath::try_from("/org/bluez").unwrap(),
            HashMap::from([("org.bluez.AgentManager1".into(), HashMap::new())]),
        );
        match parse_bluez_objects(&objects) {
            BluetoothState::Unavailable(msg) => assert!(msg.contains("No Bluetooth adapter")),
            BluetoothState::Available { .. } => panic!("expected unavailable without hci adapter"),
        }
    }

    #[test]
    fn maps_bluez_errors_cleanly() {
        assert!(map_bluez_error("org.bluez.Error.NotReady").contains("Turn on Bluetooth"));
        assert!(map_bluez_error("org.freedesktop.DBus.Error.AccessDenied").contains("bluetooth group"));
        assert!(map_bluez_error("org.freedesktop.DBus.Error.ServiceUnknown").contains("bluetoothd"));
    }
}
