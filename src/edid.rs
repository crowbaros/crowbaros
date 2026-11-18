//! A monitor's identity, from its EDID: what the layout of displays is kept
//! by (ADR 0020), so that a monitor plugged into another port keeps its
//! mode, place, and scale.
//!
//! The identity is the maker's three letters, the model's name or product
//! code, and the serial, as text or number. Bytes that are not an EDID give
//! none, and the connector's name stands in.

/// The fixed header every EDID begins with.
const HEADER: [u8; 8] = [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];

/// A monitor's identity, or None if the bytes are not an EDID.
pub(crate) fn identity(edid: &[u8]) -> Option<String> {
    if edid.len() < 128 || edid[..8] != HEADER {
        return None;
    }
    let maker = u16::from_be_bytes([edid[8], edid[9]]);
    let letter = |shift: u16| {
        let code = (maker >> shift) & 0x1f;
        (1..=26).contains(&code).then(|| char::from(b'A' + code as u8 - 1))
    };
    let maker: String = [letter(10)?, letter(5)?, letter(0)?].into_iter().collect();
    let product = u16::from_le_bytes([edid[10], edid[11]]);
    let serial = u32::from_le_bytes([edid[12], edid[13], edid[14], edid[15]]);
    // The four display descriptors: 0xfc names the model, 0xff gives the
    // serial as text.
    let text = |kind: u8| {
        (0..4).find_map(|index| {
            let block = &edid[54 + 18 * index..54 + 18 * (index + 1)];
            (block[..3] == [0, 0, 0] && block[3] == kind).then(|| {
                block[5..]
                    .iter()
                    .take_while(|byte| **byte != b'\n' && **byte != 0)
                    .filter(|byte| byte.is_ascii_graphic() || **byte == b' ')
                    .map(|byte| char::from(*byte))
                    .collect::<String>()
                    .trim()
                    .to_owned()
            })
        })
    };
    let model = text(0xfc)
        .filter(|name| !name.is_empty())
        .unwrap_or(format!("{product:04x}"));
    let serial = text(0xff)
        .filter(|serial| !serial.is_empty())
        .unwrap_or(format!("{serial}"));
    Some(format!("{maker} {model} {serial}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An EDID of a made-up monitor: maker "CRB", product 0x1234, serial
    /// 777, and descriptors as given.
    fn edid(name: Option<&str>, serial: Option<&str>) -> Vec<u8> {
        let mut bytes = vec![0u8; 128];
        bytes[..8].copy_from_slice(&HEADER);
        let code = |letter: u8| u16::from(letter - b'A' + 1);
        let maker = (code(b'C') << 10) | (code(b'R') << 5) | code(b'B');
        bytes[8..10].copy_from_slice(&maker.to_be_bytes());
        bytes[10..12].copy_from_slice(&0x1234u16.to_le_bytes());
        bytes[12..16].copy_from_slice(&777u32.to_le_bytes());
        for (index, (kind, text)) in [(0xfc, name), (0xff, serial)].into_iter().enumerate() {
            let Some(text) = text else { continue };
            let at = 54 + 18 * index;
            bytes[at + 3] = kind;
            let mut field = [b' '; 13];
            field[..text.len()].copy_from_slice(text.as_bytes());
            if text.len() < 13 {
                field[text.len()] = b'\n';
            }
            bytes[at + 5..at + 18].copy_from_slice(&field);
        }
        bytes
    }

    #[test]
    fn a_monitor_is_its_maker_model_and_serial() {
        assert_eq!(
            identity(&edid(Some("Studio 27"), Some("SN0042"))).as_deref(),
            Some("CRB Studio 27 SN0042")
        );
        assert_eq!(identity(&edid(None, None)).as_deref(), Some("CRB 1234 777"));
    }

    #[test]
    fn this_machines_monitors_have_identities() {
        let edids: Vec<Vec<u8>> = std::fs::read_dir("/sys/class/drm")
            .into_iter()
            .flatten()
            .filter_map(|entry| std::fs::read(entry.ok()?.path().join("edid")).ok())
            .filter(|bytes| !bytes.is_empty())
            .collect();
        if edids.is_empty() {
            eprintln!("no monitor: skipped");
        }
        for bytes in edids {
            let found = identity(&bytes);
            assert!(found.is_some());
        }
    }

    #[test]
    fn what_is_not_an_edid_has_no_identity_and_never_a_panic() {
        let good = edid(Some("Studio 27"), Some("SN0042"));
        assert!(identity(&good[..100]).is_none());
        assert!(identity(&[0; 128]).is_none());
        let mut seed = 11u32;
        for _ in 0..2000 {
            let mut broken = good.clone();
            for _ in 0..6 {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                let at = (seed as usize) % broken.len();
                broken[at] = (seed >> 16) as u8;
            }
            let _ = identity(&broken);
        }
    }
}
