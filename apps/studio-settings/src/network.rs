// SPDX-License-Identifier: MIT
//! Network inspection and Wi-Fi management via `ip -j` and `iwd` D-Bus (`net.connman.iwd`).
//!
//! Passphrases are delivered to `iwd` strictly in memory through a temporary
//! `net.connman.iwd.Agent` D-Bus callback and are never written to logs, error
//! strings, files, or command-line arguments.

use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use zbus::blocking::{Connection, Proxy};
use zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

const IWD_DEST: &str = "net.connman.iwd";
const IWD_AGENT_PATH: &str = "/com/crowbaros/StudioSettings/IwdAgent";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    Up,
    Down,
    Unknown,
}

impl LinkState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Up => "Connected",
            Self::Down => "Disconnected",
            Self::Unknown => "Unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EthernetInterface {
    pub name: String,
    pub state: LinkState,
    pub addresses: Vec<String>,
    pub mac: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WifiSecurity {
    Open,
    Psk,
    Enterprise,
    Unknown(String),
}

impl WifiSecurity {
    pub fn from_iwd(kind: &str) -> Self {
        match kind.to_ascii_lowercase().as_str() {
            "open" => Self::Open,
            "psk" | "wep" => Self::Psk,
            "8021x" => Self::Enterprise,
            other => Self::Unknown(other.to_string()),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Psk => "WPA",
            Self::Enterprise => "802.1X",
            Self::Unknown(_) => "Secured",
        }
    }

    pub fn needs_passphrase(&self) -> bool {
        matches!(self, Self::Psk)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiNetwork {
    pub path: String,
    pub ssid: String,
    pub security: WifiSecurity,
    /// Signal strength in dBm (converted from iwd's hundredths of dBm).
    pub signal_dbm: Option<i16>,
    pub connected: bool,
    pub known: bool,
    pub known_network_path: Option<String>,
}

impl WifiNetwork {
    pub fn signal_label(&self) -> String {
        match self.signal_dbm {
            Some(dbm) => format!("{dbm} dBm · {}", signal_quality(dbm)),
            None => "Signal unknown".into(),
        }
    }
}

pub fn signal_quality(dbm: i16) -> &'static str {
    if dbm >= -55 {
        "Excellent"
    } else if dbm >= -67 {
        "Good"
    } else if dbm >= -78 {
        "Fair"
    } else {
        "Weak"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiStation {
    pub device_path: String,
    pub device_name: String,
    pub powered: bool,
    pub state: String,
    pub scanning: bool,
    pub connected_ssid: Option<String>,
    pub addresses: Vec<String>,
    pub networks: Vec<WifiNetwork>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WifiState {
    Available(WifiStation),
    Unavailable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkSnapshot {
    pub ethernet: Vec<EthernetInterface>,
    pub wifi: WifiState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkError {
    Failed(String),
}

impl NetworkError {
    pub fn message(&self) -> &str {
        match self {
            Self::Failed(msg) => msg,
        }
    }
}

type PropMap = HashMap<String, OwnedValue>;
pub type ManagedObjects = HashMap<OwnedObjectPath, HashMap<String, PropMap>>;

#[derive(Debug, Deserialize)]
struct IpLinkJson {
    ifname: String,
    #[serde(default)]
    operstate: String,
    #[serde(default)]
    link_type: String,
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    flags: Vec<String>,
    #[serde(default)]
    addr_info: Vec<IpAddrInfoJson>,
}

#[derive(Debug, Deserialize)]
struct IpAddrInfoJson {
    family: String,
    local: String,
    prefixlen: u8,
    #[serde(default)]
    scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceInfo {
    pub name: String,
    pub state: LinkState,
    pub addresses: Vec<String>,
    pub mac: Option<String>,
    pub is_wireless: bool,
}

pub fn parse_ip_addr_json(
    raw: &str,
    wireless_ifaces: &HashSet<String>,
) -> Result<Vec<InterfaceInfo>, String> {
    let links: Vec<IpLinkJson> =
        serde_json::from_str(raw).map_err(|e| format!("Could not parse `ip -j addr` output: {e}"))?;
    let mut out = Vec::new();
    for link in links {
        if link.link_type == "loopback" || link.ifname == "lo" {
            continue;
        }
        // Only surface Ethernet-class interfaces (wired NICs & Wi-Fi radios).
        if link.link_type != "ether" {
            continue;
        }
        let has_carrier = link.flags.iter().any(|f| f == "LOWER_UP")
            && !link.flags.iter().any(|f| f == "NO-CARRIER");
        let state = match link.operstate.to_ascii_uppercase().as_str() {
            "UP" => LinkState::Up,
            "DOWN" | "LOWERLAYERDOWN" | "NOTPRESENT" => LinkState::Down,
            _ if has_carrier => LinkState::Up,
            _ => LinkState::Unknown,
        };
        let mut addresses = Vec::new();
        for addr in link.addr_info {
            if addr.scope == "link" || addr.scope == "host" {
                continue;
            }
            if addr.family == "inet" || addr.family == "inet6" {
                addresses.push(format!("{}/{}", addr.local, addr.prefixlen));
            }
        }
        let is_wireless = wireless_ifaces.contains(&link.ifname);
        out.push(InterfaceInfo {
            name: link.ifname,
            state,
            addresses,
            mac: link.address,
            is_wireless,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn detect_wireless_interfaces() -> HashSet<String> {
    let mut names = HashSet::new();
    if let Ok(entries) = std::fs::read_dir("/sys/class/net") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if Path::new("/sys/class/net").join(&name).join("wireless").is_dir() {
                names.insert(name);
            }
        }
    }
    names
}

fn fetch_interfaces(extra_wireless: &HashSet<String>) -> Vec<InterfaceInfo> {
    let mut wireless = detect_wireless_interfaces();
    wireless.extend(extra_wireless.iter().cloned());
    let Ok(output) = Command::new("ip").args(["-j", "addr", "show"]).output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_ip_addr_json(&stdout, &wireless).unwrap_or_default()
}

fn get_string(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    let value = props.get(key)?;
    <&str>::try_from(value).ok().map(str::to_string)
}

fn get_bool(props: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    let value = props.get(key)?;
    bool::try_from(value).ok()
}

fn get_object_path(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    let value = props.get(key)?;
    match &**value {
        Value::ObjectPath(p) => Some(p.as_str().to_string()),
        _ => None,
    }
}

pub fn parse_iwd_objects(
    objects: &ManagedObjects,
    ordered_signals: &HashMap<String, i16>,
    iface_addrs: &HashMap<String, Vec<String>>,
) -> WifiState {
    // Collect stations/devices.
    let mut stations: Vec<(&str, &PropMap, &PropMap)> = Vec::new();
    for (path, ifaces) in objects {
        if let (Some(device), Some(station)) = (
            ifaces.get("net.connman.iwd.Device"),
            ifaces.get("net.connman.iwd.Station"),
        ) {
            stations.push((path.as_str(), device, station));
        }
    }
    stations.sort_by_key(|(path, _, _)| *path);

    let Some((device_path, device_props, station_props)) = stations.into_iter().next() else {
        return WifiState::Unavailable("No Wi-Fi adapter detected on this machine.".into());
    };

    let device_name = get_string(device_props, "Name").unwrap_or_else(|| "wlan0".into());
    let powered = get_bool(device_props, "Powered").unwrap_or(true);
    let state = get_string(station_props, "State").unwrap_or_else(|| "disconnected".into());
    let scanning = get_bool(station_props, "Scanning").unwrap_or(false);
    let connected_path = get_object_path(station_props, "ConnectedNetwork");

    // Collect visible networks for this station.
    let mut networks = Vec::new();
    let mut connected_ssid = None;
    for (path, ifaces) in objects {
        let Some(net_props) = ifaces.get("net.connman.iwd.Network") else {
            continue;
        };
        if let Some(net_dev) = get_object_path(net_props, "Device")
            && net_dev != device_path
        {
            continue;
        }
        let Some(ssid) = get_string(net_props, "Name") else {
            continue;
        };
        let security = WifiSecurity::from_iwd(
            &get_string(net_props, "Type").unwrap_or_else(|| "psk".into()),
        );
        let known_network_path = get_object_path(net_props, "KnownNetwork");
        let known = known_network_path.is_some();
        let connected = get_bool(net_props, "Connected").unwrap_or(false)
            || connected_path.as_deref() == Some(path.as_str());
        if connected {
            connected_ssid = Some(ssid.clone());
        }
        let signal_dbm = ordered_signals.get(path.as_str()).map(|s| *s / 100);
        networks.push(WifiNetwork {
            path: path.as_str().to_string(),
            ssid,
            security,
            signal_dbm,
            connected,
            known,
            known_network_path,
        });
    }

    // Connected first, then strongest signal, then alphabetical SSID.
    networks.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then_with(|| b.signal_dbm.cmp(&a.signal_dbm))
            .then_with(|| a.ssid.to_lowercase().cmp(&b.ssid.to_lowercase()))
    });

    let addresses = iface_addrs.get(&device_name).cloned().unwrap_or_default();

    WifiState::Available(WifiStation {
        device_path: device_path.to_string(),
        device_name,
        powered,
        state,
        scanning,
        connected_ssid,
        addresses,
        networks,
    })
}

pub fn map_iwd_error(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("serviceunknown") || lower.contains("namehasnoowner") {
        "Wi-Fi service (iwd) is not running.".into()
    } else if lower.contains("net.connman.iwd.inprogress") {
        "A Wi-Fi operation is already in progress.".into()
    } else if lower.contains("net.connman.iwd.busy") {
        "Wi-Fi adapter is busy. Try again in a moment.".into()
    } else if lower.contains("net.connman.iwd.invalidarguments")
        || lower.contains("net.connman.iwd.invalidformat")
    {
        "Passphrase format was rejected by the network.".into()
    } else if lower.contains("net.connman.iwd.canceled") || lower.contains("net.connman.iwd.aborted")
    {
        "Wi-Fi connection was canceled.".into()
    } else if lower.contains("net.connman.iwd.failed") {
        "Could not connect to this Wi-Fi network. Check the passphrase and signal.".into()
    } else if lower.contains("accessdenied") || lower.contains("permission") {
        "Permission denied by iwd. Your account must belong to the netdev group.".into()
    } else {
        "Wi-Fi request failed. Try refreshing the network list.".into()
    }
}

pub fn mask_passphrase(passphrase: &str, reveal: bool) -> String {
    if reveal {
        passphrase.to_string()
    } else {
        "•".repeat(passphrase.chars().count())
    }
}

fn is_service_missing(err: &zbus::Error) -> bool {
    let text = err.to_string();
    text.contains("ServiceUnknown")
        || text.contains("NameHasNoOwner")
        || text.contains("No such file or directory")
}

pub fn fetch_network() -> NetworkSnapshot {
    let mut iwd_device_names = HashSet::new();
    let iwd_data = fetch_iwd_raw();
    if let Ok((objects, _)) = &iwd_data {
        for ifaces in objects.values() {
            if let Some(dev) = ifaces.get("net.connman.iwd.Device")
                && let Some(name) = get_string(dev, "Name")
            {
                iwd_device_names.insert(name);
            }
        }
    }

    let interfaces = fetch_interfaces(&iwd_device_names);
    let mut iface_addrs = HashMap::new();
    let mut ethernet = Vec::new();
    for iface in interfaces {
        iface_addrs.insert(iface.name.clone(), iface.addresses.clone());
        if !iface.is_wireless {
            ethernet.push(EthernetInterface {
                name: iface.name,
                state: iface.state,
                addresses: iface.addresses,
                mac: iface.mac,
            });
        }
    }

    let wifi = match iwd_data {
        Ok((objects, ordered)) => parse_iwd_objects(&objects, &ordered, &iface_addrs),
        Err(reason) => WifiState::Unavailable(reason),
    };

    NetworkSnapshot { ethernet, wifi }
}

fn fetch_iwd_raw() -> Result<(ManagedObjects, HashMap<String, i16>), String> {
    let conn = Connection::system()
        .map_err(|_| "System D-Bus is unavailable on this machine.".to_string())?;
    let proxy = Proxy::new(&conn, IWD_DEST, "/", "org.freedesktop.DBus.ObjectManager")
        .map_err(|e| map_iwd_error(&e.to_string()))?;
    let objects: ManagedObjects = proxy.call("GetManagedObjects", &()).map_err(|err| {
        if is_service_missing(&err) {
            "Wi-Fi service (iwd) is not running.".to_string()
        } else {
            map_iwd_error(&err.to_string())
        }
    })?;

    let mut ordered = HashMap::new();
    for (path, ifaces) in &objects {
        if ifaces.contains_key("net.connman.iwd.Station")
            && let Ok(station_proxy) =
                Proxy::new(&conn, IWD_DEST, path.as_str(), "net.connman.iwd.Station")
            && let Ok(list) = station_proxy.call::<_, _, Vec<(OwnedObjectPath, i16)>>(
                "GetOrderedNetworks",
                &(),
            )
        {
            for (net_path, strength) in list {
                ordered.insert(net_path.as_str().to_string(), strength);
            }
        }
    }

    Ok((objects, ordered))
}

pub fn scan_wifi(device_path: &str) -> Result<(), NetworkError> {
    let conn = Connection::system()
        .map_err(|e| NetworkError::Failed(map_iwd_error(&e.to_string())))?;
    let proxy = Proxy::new(&conn, IWD_DEST, device_path, "net.connman.iwd.Station")
        .map_err(|e| NetworkError::Failed(map_iwd_error(&e.to_string())))?;
    match proxy.call::<_, _, ()>("Scan", &()) {
        Ok(()) => Ok(()),
        Err(err) if err.to_string().contains("InProgress") => Ok(()),
        Err(err) => Err(NetworkError::Failed(map_iwd_error(&err.to_string()))),
    }
}

pub fn disconnect_wifi(device_path: &str) -> Result<(), NetworkError> {
    let conn = Connection::system()
        .map_err(|e| NetworkError::Failed(map_iwd_error(&e.to_string())))?;
    let proxy = Proxy::new(&conn, IWD_DEST, device_path, "net.connman.iwd.Station")
        .map_err(|e| NetworkError::Failed(map_iwd_error(&e.to_string())))?;
    proxy
        .call::<_, _, ()>("Disconnect", &())
        .map_err(|e| NetworkError::Failed(map_iwd_error(&e.to_string())))
}

pub fn forget_wifi(known_network_path: &str) -> Result<(), NetworkError> {
    let conn = Connection::system()
        .map_err(|e| NetworkError::Failed(map_iwd_error(&e.to_string())))?;
    let proxy = Proxy::new(
        &conn,
        IWD_DEST,
        known_network_path,
        "net.connman.iwd.KnownNetwork",
    )
    .map_err(|e| NetworkError::Failed(map_iwd_error(&e.to_string())))?;
    proxy
        .call::<_, _, ()>("Forget", &())
        .map_err(|e| NetworkError::Failed(map_iwd_error(&e.to_string())))
}

struct IwdPassphraseAgent {
    passphrase: Mutex<Option<String>>,
}

#[zbus::interface(name = "net.connman.iwd.Agent")]
impl IwdPassphraseAgent {
    fn release(&self) {
        if let Ok(mut guard) = self.passphrase.lock() {
            guard.take();
        }
    }

    fn request_passphrase(&self, _network: ObjectPath<'_>) -> zbus::fdo::Result<String> {
        let mut guard = self
            .passphrase
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard
            .take()
            .ok_or_else(|| zbus::fdo::Error::Failed("No passphrase supplied".into()))
    }

    fn cancel(&self, _reason: String) {
        if let Ok(mut guard) = self.passphrase.lock() {
            guard.take();
        }
    }
}

pub fn connect_wifi(network_path: &str, passphrase: Option<String>) -> Result<(), NetworkError> {
    let conn = Connection::system()
        .map_err(|e| NetworkError::Failed(map_iwd_error(&e.to_string())))?;

    let mut agent_registered = false;
    if let Some(secret) = passphrase {
        let agent = IwdPassphraseAgent {
            passphrase: Mutex::new(Some(secret)),
        };
        conn.object_server()
            .at(IWD_AGENT_PATH, agent)
            .map_err(|e| NetworkError::Failed(map_iwd_error(&e.to_string())))?;
        let mgr = Proxy::new(&conn, IWD_DEST, "/net/connman/iwd", "net.connman.iwd.AgentManager")
            .map_err(|e| NetworkError::Failed(map_iwd_error(&e.to_string())))?;
        let path = ObjectPath::try_from(IWD_AGENT_PATH)
            .map_err(|e| NetworkError::Failed(map_iwd_error(&e.to_string())))?;
        mgr.call::<_, _, ()>("RegisterAgent", &(path,))
            .map_err(|e| NetworkError::Failed(map_iwd_error(&e.to_string())))?;
        agent_registered = true;
    }

    let net_proxy = Proxy::new(&conn, IWD_DEST, network_path, "net.connman.iwd.Network")
        .map_err(|e| NetworkError::Failed(map_iwd_error(&e.to_string())))?;
    let connect_result = net_proxy.call::<_, _, ()>("Connect", &());

    if agent_registered {
        if let Ok(mgr) =
            Proxy::new(&conn, IWD_DEST, "/net/connman/iwd", "net.connman.iwd.AgentManager")
            && let Ok(path) = ObjectPath::try_from(IWD_AGENT_PATH)
        {
            let _ = mgr.call::<_, _, ()>("UnregisterAgent", &(path,));
        }
        let _ = conn.object_server().remove::<IwdPassphraseAgent, _>(IWD_AGENT_PATH);
    }

    connect_result.map_err(|e| NetworkError::Failed(map_iwd_error(&e.to_string())))
}

#[allow(dead_code)]
fn _keep_value_import_used(_: Value<'_>) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned_str(s: &str) -> OwnedValue {
        Value::from(s).try_to_owned().unwrap()
    }

    fn owned_bool(b: bool) -> OwnedValue {
        Value::from(b).try_to_owned().unwrap()
    }

    fn owned_path(p: &str) -> OwnedValue {
        Value::from(ObjectPath::try_from(p).unwrap())
            .try_to_owned()
            .unwrap()
    }

    #[test]
    fn parses_ethernet_and_separates_wireless_and_loopback() {
        let json = r#"[
            {"ifname":"lo","operstate":"UNKNOWN","link_type":"loopback","flags":["LOOPBACK","UP"],"addr_info":[{"family":"inet","local":"127.0.0.1","prefixlen":8,"scope":"host"}]},
            {"ifname":"eth0","operstate":"UP","link_type":"ether","address":"52:54:00:12:34:56","flags":["BROADCAST","UP","LOWER_UP"],"addr_info":[
                {"family":"inet","local":"10.0.2.15","prefixlen":24,"scope":"global"},
                {"family":"inet6","local":"fe80::5054:ff:fe12:3456","prefixlen":64,"scope":"link"}
            ]},
            {"ifname":"enp2s0","operstate":"DOWN","link_type":"ether","address":"aa:bb:cc:dd:ee:ff","flags":["NO-CARRIER","BROADCAST"],"addr_info":[]},
            {"ifname":"wlan0","operstate":"UP","link_type":"ether","address":"11:22:33:44:55:66","flags":["UP","LOWER_UP"],"addr_info":[
                {"family":"inet","local":"192.0.2.42","prefixlen":24,"scope":"global"}
            ]}
        ]"#;
        let wireless = HashSet::from(["wlan0".to_string()]);
        let parsed = parse_ip_addr_json(json, &wireless).unwrap();
        assert_eq!(parsed.len(), 3);
        let eth0 = parsed.iter().find(|i| i.name == "eth0").unwrap();
        assert_eq!(eth0.state, LinkState::Up);
        assert_eq!(eth0.addresses, vec!["10.0.2.15/24"]);
        assert!(!eth0.is_wireless);

        let enp = parsed.iter().find(|i| i.name == "enp2s0").unwrap();
        assert_eq!(enp.state, LinkState::Down);
        assert!(enp.addresses.is_empty());

        let wlan0 = parsed.iter().find(|i| i.name == "wlan0").unwrap();
        assert!(wlan0.is_wireless);
        assert_eq!(wlan0.addresses, vec!["192.0.2.42/24"]);
    }

    #[test]
    fn parses_iwd_objects_with_ordered_networks_and_known_flag() {
        let mut objects: ManagedObjects = HashMap::new();
        let dev_path = OwnedObjectPath::try_from("/net/connman/iwd/0/2").unwrap();
        let net1 = OwnedObjectPath::try_from("/net/connman/iwd/0/2/net_studio").unwrap();
        let net2 = OwnedObjectPath::try_from("/net/connman/iwd/0/2/net_cafe").unwrap();
        let known1 = OwnedObjectPath::try_from("/net/connman/iwd/known_studio").unwrap();

        objects.insert(
            dev_path.clone(),
            HashMap::from([
                (
                    "net.connman.iwd.Device".into(),
                    HashMap::from([
                        ("Name".into(), owned_str("wlan0")),
                        ("Powered".into(), owned_bool(true)),
                    ]),
                ),
                (
                    "net.connman.iwd.Station".into(),
                    HashMap::from([
                        ("State".into(), owned_str("connected")),
                        ("Scanning".into(), owned_bool(false)),
                        ("ConnectedNetwork".into(), owned_path(net1.as_str())),
                    ]),
                ),
            ]),
        );
        objects.insert(
            net1.clone(),
            HashMap::from([(
                "net.connman.iwd.Network".into(),
                HashMap::from([
                    ("Name".into(), owned_str("StudioNet")),
                    ("Type".into(), owned_str("psk")),
                    ("Connected".into(), owned_bool(true)),
                    ("Device".into(), owned_path(dev_path.as_str())),
                    ("KnownNetwork".into(), owned_path(known1.as_str())),
                ]),
            )]),
        );
        objects.insert(
            net2.clone(),
            HashMap::from([(
                "net.connman.iwd.Network".into(),
                HashMap::from([
                    ("Name".into(), owned_str("GuestOpen")),
                    ("Type".into(), owned_str("open")),
                    ("Connected".into(), owned_bool(false)),
                    ("Device".into(), owned_path(dev_path.as_str())),
                ]),
            )]),
        );

        let ordered = HashMap::from([
            (net1.as_str().to_string(), -5200),
            (net2.as_str().to_string(), -7100),
        ]);
        let addrs = HashMap::from([("wlan0".to_string(), vec!["192.0.2.10/24".to_string()])]);

        let WifiState::Available(station) = parse_iwd_objects(&objects, &ordered, &addrs) else {
            panic!("expected available Wi-Fi station");
        };
        assert_eq!(station.device_name, "wlan0");
        assert_eq!(station.connected_ssid.as_deref(), Some("StudioNet"));
        assert_eq!(station.addresses, vec!["192.0.2.10/24"]);
        assert_eq!(station.networks.len(), 2);
        assert_eq!(station.networks[0].ssid, "StudioNet");
        assert!(station.networks[0].connected);
        assert!(station.networks[0].known);
        assert_eq!(station.networks[0].signal_dbm, Some(-52));
        assert_eq!(station.networks[1].ssid, "GuestOpen");
        assert_eq!(station.networks[1].security, WifiSecurity::Open);
        assert!(!station.networks[1].security.needs_passphrase());
    }

    #[test]
    fn empty_iwd_objects_yields_calm_unavailable_state() {
        let state = parse_iwd_objects(&HashMap::new(), &HashMap::new(), &HashMap::new());
        match state {
            WifiState::Unavailable(msg) => assert!(msg.contains("No Wi-Fi adapter")),
            WifiState::Available(_) => panic!("expected unavailable state without adapter"),
        }
    }

    #[test]
    fn maps_iwd_errors_without_echoing_raw_details_or_secrets() {
        let err = map_iwd_error("net.connman.iwd.Failed: secret-passphrase-123");
        assert!(!err.contains("secret-passphrase-123"));
        assert!(err.contains("Could not connect"));
        assert!(map_iwd_error("org.freedesktop.DBus.Error.AccessDenied").contains("netdev"));
        assert!(map_iwd_error("org.freedesktop.DBus.Error.ServiceUnknown").contains("iwd"));
    }

    #[test]
    fn masks_passphrase_by_default_and_reveals_on_toggle() {
        assert_eq!(mask_passphrase("hunter2", false), "•••••••");
        assert_eq!(mask_passphrase("hunter2", true), "hunter2");
        assert_eq!(mask_passphrase("", false), "");
    }
}
