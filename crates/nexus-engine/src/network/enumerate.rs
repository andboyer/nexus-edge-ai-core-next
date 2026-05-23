//! Cross-platform NIC enumeration backing
//! `GET /v1/admin/network/interfaces` and the bind-by-interface
//! dropdowns in the Server Settings page.
//!
//! `if-addrs` gives us names + bound IPv4/IPv6 + netmask + a
//! cheap MAC; on Linux we augment from `/sys/class/net/<name>/*`
//! to also surface link-state (operstate / carrier), MTU, and
//! whether the interface is a VLAN sub-interface (and if so,
//! which parent + tag id). Everything that requires elevation
//! (rtnetlink, ioctl SIOCETHTOOL, etc.) is deliberately out of
//! scope here — the helper binary handles those.

use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::fs;
use std::net::IpAddr;

use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NicError {
    #[error("if-addrs: {0}")]
    IfAddrs(#[from] std::io::Error),
}

/// One bound address on an interface. `prefix_len` is the CIDR
/// prefix length (e.g. 24 for a /24). The UI shows
/// `<ip>/<prefix>` next to each NIC.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InterfaceAddr {
    pub addr: IpAddr,
    pub prefix_len: u8,
    /// `"ipv4"` or `"ipv6"`. Pre-computed for the UI so it
    /// doesn't have to inspect `addr` to filter the list.
    pub family: &'static str,
}

/// Snapshot of one OS-level network interface. `kind` and
/// `parent` are populated on Linux only — on macOS dev they
/// stay `Physical` / `None` regardless of what the interface
/// actually is, since the Phase B mutation path only runs on
/// Linux anyway.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NetworkInterface {
    /// Kernel-assigned name, e.g. `eno1`, `wlan0`, `en0`,
    /// `eno1.20` (VLAN), `lo`. Used verbatim as the netplan
    /// key.
    pub name: String,
    /// Hex-formatted MAC (`aa:bb:cc:dd:ee:ff`). `None` for
    /// loopback / interfaces that don't have one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
    /// Bound IPv4 + IPv6 addresses. Sorted ipv4-first so the
    /// dropdown's "primary" address (first listed) is the v4.
    pub addrs: Vec<InterfaceAddr>,
    /// True for `lo` / `lo0` — UI filters these out of the
    /// bind dropdowns by default.
    pub is_loopback: bool,
    /// Kernel operstate. `Some("up")` / `Some("down")` /
    /// `Some("unknown")` on Linux; `None` on macOS.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operstate: Option<String>,
    /// `Some(true)` when the kernel reports a carrier (cable
    /// plugged / radio associated). `None` on macOS.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub carrier: Option<bool>,
    /// MTU in bytes. `None` on macOS.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtu: Option<u32>,
    /// Classified interface type. Defaults to `Physical` on
    /// macOS regardless of the actual class.
    pub kind: InterfaceKind,
    /// For `Vlan` interfaces, the parent interface name
    /// (`eno1` for `eno1.20`). `None` for everything else.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// For `Vlan` interfaces, the 802.1Q VLAN id (1–4094).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vlan_id: Option<u16>,
}

/// Coarse classification of an interface for the UI. Drives the
/// "Add VLAN" button (only on `Physical`) and the delete affordance
/// (only on `Vlan`).
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceKind {
    Physical,
    Vlan,
    Bridge,
    Bond,
    Wireless,
    Loopback,
    Other,
}

/// Walk every interface the OS knows about. The result is sorted
/// by name so the UI dropdown is stable across refreshes.
pub fn list_interfaces() -> Result<Vec<NetworkInterface>, NicError> {
    // if-addrs gives us name + addr + netmask + a MAC. We bucket
    // multiple addrs per interface ourselves (if-addrs yields one
    // `Interface` per addr).
    let raw = if_addrs::get_if_addrs()?;
    let mut by_name: BTreeMap<String, NetworkInterface> = BTreeMap::new();

    for ifa in raw {
        let is_loopback = ifa.is_loopback();
        let entry = by_name
            .entry(ifa.name.clone())
            .or_insert_with(|| NetworkInterface {
                name: ifa.name.clone(),
                mac: None,
                addrs: Vec::new(),
                is_loopback,
                operstate: None,
                carrier: None,
                mtu: None,
                kind: classify(&ifa.name, is_loopback),
                parent: None,
                vlan_id: None,
            });

        let prefix_len = match &ifa.addr {
            if_addrs::IfAddr::V4(v4) => prefix_from_v4_mask(v4.netmask.octets()),
            if_addrs::IfAddr::V6(v6) => prefix_from_v6_mask(v6.netmask.octets()),
        };
        let family = match ifa.addr {
            if_addrs::IfAddr::V4(_) => "ipv4",
            if_addrs::IfAddr::V6(_) => "ipv6",
        };
        entry.addrs.push(InterfaceAddr {
            addr: ifa.ip(),
            prefix_len,
            family,
        });
    }

    // Sort each NIC's addr list IPv4-first so the UI dropdown's
    // "primary" is the v4 address.
    for nic in by_name.values_mut() {
        nic.addrs.sort_by_key(|a| {
            if matches!(a.addr, IpAddr::V4(_)) {
                0
            } else {
                1
            }
        });
    }

    // On Linux, augment from /sys/class/net/<name>/*. Cheap reads,
    // no syscalls beyond open + read. Failures are ignored — the
    // resulting NIC just has `None` for the augmented fields.
    #[cfg(target_os = "linux")]
    {
        for nic in by_name.values_mut() {
            augment_from_sysfs(nic);
        }
    }

    Ok(by_name.into_values().collect())
}

/// Heuristic name → kind. Linux's `/sys/class/net/<name>/type`
/// is the authoritative source but it's just an integer
/// (`ARPHRD_*`); for a UI hint, name-prefix is plenty.
fn classify(name: &str, is_loopback: bool) -> InterfaceKind {
    if is_loopback {
        return InterfaceKind::Loopback;
    }
    // VLAN: `eno1.20`, `eth0.100`, etc. The dot is the
    // netplan + iproute2 convention.
    if name.contains('.') {
        return InterfaceKind::Vlan;
    }
    if name.starts_with("br") || name.starts_with("docker") || name.starts_with("virbr") {
        return InterfaceKind::Bridge;
    }
    if name.starts_with("bond") {
        return InterfaceKind::Bond;
    }
    if name.starts_with("wl") || name.starts_with("wlan") {
        return InterfaceKind::Wireless;
    }
    // Common physical-NIC name prefixes (systemd predictable names +
    // legacy `eth*` + macOS `en*`).
    if name.starts_with("eno")
        || name.starts_with("ens")
        || name.starts_with("enp")
        || name.starts_with("enx")
        || name.starts_with("eth")
        || name.starts_with("en")
    {
        return InterfaceKind::Physical;
    }
    InterfaceKind::Other
}

#[cfg(target_os = "linux")]
fn augment_from_sysfs(nic: &mut NetworkInterface) {
    let base = format!("/sys/class/net/{}", nic.name);

    if nic.mac.is_none() {
        if let Ok(s) = fs::read_to_string(format!("{base}/address")) {
            let s = s.trim().to_string();
            if !s.is_empty() && s != "00:00:00:00:00:00" {
                nic.mac = Some(s);
            }
        }
    }
    if let Ok(s) = fs::read_to_string(format!("{base}/operstate")) {
        nic.operstate = Some(s.trim().to_string());
    }
    if let Ok(s) = fs::read_to_string(format!("{base}/carrier")) {
        nic.carrier = Some(s.trim() == "1");
    }
    if let Ok(s) = fs::read_to_string(format!("{base}/mtu")) {
        nic.mtu = s.trim().parse::<u32>().ok();
    }

    // VLAN sub-interface info. `/proc/net/vlan/<name>` is the
    // canonical source — it lists parent + vlan id. We also fall
    // back to parsing the name for the tag if the proc entry is
    // missing (e.g. CONFIG_VLAN_8021Q built-in but procfs not
    // mounted).
    if matches!(nic.kind, InterfaceKind::Vlan) {
        if let Ok(s) = fs::read_to_string(format!("/proc/net/vlan/{}", nic.name)) {
            for line in s.lines() {
                if let Some(rest) = line.strip_prefix("Device:") {
                    nic.parent = Some(rest.trim().to_string());
                }
                if let Some(rest) = line.strip_prefix("VID:") {
                    if let Some(id) = rest.split_whitespace().next() {
                        nic.vlan_id = id.parse().ok();
                    }
                }
            }
        }
        if nic.parent.is_none() {
            if let Some((parent, tag)) = nic.name.split_once('.') {
                nic.parent = Some(parent.to_string());
                nic.vlan_id = tag.parse().ok();
            }
        }
    }
}

fn prefix_from_v4_mask(octets: [u8; 4]) -> u8 {
    let bits = u32::from_be_bytes(octets);
    bits.count_ones() as u8
}

fn prefix_from_v6_mask(octets: [u8; 16]) -> u8 {
    let mut p = 0u8;
    for o in octets {
        p += o.count_ones() as u8;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity check — every OS we run on has at least a loopback
    /// interface. Regressions in `if-addrs` would surface here as
    /// either an empty list or a missing-name field.
    #[test]
    fn lists_at_least_loopback() {
        let nics = list_interfaces().expect("if-addrs read failed");
        assert!(!nics.is_empty(), "expected ≥1 NIC, got empty list");
        assert!(
            nics.iter().any(|n| n.is_loopback),
            "expected at least one loopback, got: {:#?}",
            nics.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn classify_known_prefixes() {
        assert!(matches!(classify("lo", true), InterfaceKind::Loopback));
        assert!(matches!(classify("eno1", false), InterfaceKind::Physical));
        assert!(matches!(classify("enp3s0", false), InterfaceKind::Physical));
        assert!(matches!(classify("ens18", false), InterfaceKind::Physical));
        assert!(matches!(classify("en0", false), InterfaceKind::Physical));
        assert!(matches!(classify("eno1.20", false), InterfaceKind::Vlan));
        assert!(matches!(classify("wlan0", false), InterfaceKind::Wireless));
        assert!(matches!(classify("br0", false), InterfaceKind::Bridge));
        assert!(matches!(classify("bond0", false), InterfaceKind::Bond));
    }

    #[test]
    fn prefix_from_v4_mask_known_values() {
        assert_eq!(prefix_from_v4_mask([255, 255, 255, 0]), 24);
        assert_eq!(prefix_from_v4_mask([255, 255, 255, 255]), 32);
        assert_eq!(prefix_from_v4_mask([255, 255, 252, 0]), 22);
        assert_eq!(prefix_from_v4_mask([0, 0, 0, 0]), 0);
    }
}
