//! Operator-facing network plan: a small, opinionated subset of
//! netplan v2's YAML schema that the engine round-trips through
//! `engine_runtime_settings.network_plan_json` (for audit + diff)
//! and through `/etc/netplan/90-nexus.yaml` (the canonical OS
//! configuration the helper binary atomically writes).
//!
//! ## Why a curated subset
//!
//! Netplan v2 supports tunnels, modems, wifis, bonds, bridges,
//! VRFs, OpenVSwitch overlays, route policies, route metric
//! tuning, multiple renderers — none of which we need on a K13
//! edge appliance. The UI exposes exactly what an operator who
//! wants to "bind the engine on a secure VLAN and the admin UI
//! on an open VLAN" actually needs:
//!
//!   - per-physical-NIC: dhcp4 OR a list of static `addr/prefix`
//!     entries + optional default-gateway IP + optional DNS list
//!     + optional MTU override + optional MAC override
//!   - VLAN sub-interfaces: id (1–4094) + parent (must be a
//!     physical NIC declared above) + the same addr/dhcp/dns/mtu
//!     shape as physical NICs
//!
//! Anything fancier than that is a sign the operator needs to
//! hand-edit `/etc/netplan/99-operator.yaml`. We deliberately do
//! NOT touch other files under `/etc/netplan/*.yaml`; netplan
//! merges every file at apply time so operator-managed files
//! co-exist with ours.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Top-level plan. Mirrors the netplan `network:` document
/// shape one-for-one so YAML round-trip is lossless for the
/// fields we expose.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetplanPlan {
    /// Physical NIC configs keyed by kernel name (`eno1`, `enp3s0`,
    /// etc). Operators can't add entries here for NICs that don't
    /// physically exist — the API rejects PUTs that name unknown
    /// interfaces.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ethernets: BTreeMap<String, EthernetConfig>,
    /// VLAN sub-interface configs keyed by the canonical name
    /// (`eno1.20`). The `link` field on each must point at a key
    /// in `ethernets`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vlans: BTreeMap<String, VlanConfig>,
}

/// One physical-NIC config. All fields are optional so an empty
/// `EthernetConfig {}` is a valid "leave this NIC alone" entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EthernetConfig {
    /// When `Some(true)` netplan enables the dhcp4 client and
    /// `addresses` is ignored. When `None` or `Some(false)` the
    /// static config in `addresses` is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dhcp4: Option<bool>,
    /// Static IPv4/IPv6 addresses in CIDR form
    /// (e.g. `["192.168.1.66/24", "fe80::1/64"]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<String>,
    /// Optional default-gateway IP. Rendered as a
    /// `routes: [{ to: default, via: <addr> }]` entry in YAML so
    /// we never emit the deprecated `gateway4` key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<IpAddr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nameservers: Option<Nameservers>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtu: Option<u32>,
    /// Override the NIC's MAC. Bare `aa:bb:cc:dd:ee:ff` form;
    /// rendered into the YAML as `macaddress: '<value>'`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub macaddress: Option<String>,
}

/// One VLAN sub-interface config.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VlanConfig {
    /// 802.1Q VLAN id, 1–4094. 0 and 4095 are reserved.
    pub id: u16,
    /// Parent interface — must be a key in `ethernets`.
    pub link: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dhcp4: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<IpAddr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nameservers: Option<Nameservers>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtu: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Nameservers {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<IpAddr>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub search: Vec<String>,
}

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("yaml: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("invalid plan: {0}")]
    Invalid(String),
}

impl NetplanPlan {
    /// Parse the operator-facing JSON shape. (Same schema as
    /// YAML — serde_json is just what the HTTP API speaks.)
    pub fn from_json(s: &str) -> Result<Self, PlanError> {
        serde_json::from_str(s).map_err(|e| PlanError::Invalid(format!("json: {e}")))
    }

    pub fn to_json(&self) -> Result<String, PlanError> {
        serde_json::to_string_pretty(self)
            .map_err(|e| PlanError::Invalid(format!("json serialise: {e}")))
    }

    /// Render to the canonical netplan YAML document.
    ///
    /// We deliberately re-shape `gateway: <addr>` into the modern
    /// `routes: [{ to: default, via: <addr> }]` form because the
    /// flat `gateway4` / `gateway6` keys have been deprecated
    /// since netplan 0.103 and emit warnings on Ubuntu 24.04.
    pub fn to_yaml(&self) -> Result<String, PlanError> {
        // Build a serde_yaml::Value so we can emit the netplan
        // top-level shape (`network: { version: 2, renderer: ... }`)
        // without leaking that envelope into our public type.
        use serde_yaml::Value;

        let mut net = serde_yaml::Mapping::new();
        net.insert("version".into(), 2.into());
        net.insert("renderer".into(), "networkd".into());

        if !self.ethernets.is_empty() {
            let mut eths = serde_yaml::Mapping::new();
            for (name, eth) in &self.ethernets {
                eths.insert(name.clone().into(), ethernet_to_yaml(eth)?);
            }
            net.insert("ethernets".into(), Value::Mapping(eths));
        }

        if !self.vlans.is_empty() {
            let mut vls = serde_yaml::Mapping::new();
            for (name, v) in &self.vlans {
                vls.insert(name.clone().into(), vlan_to_yaml(v)?);
            }
            net.insert("vlans".into(), Value::Mapping(vls));
        }

        let mut root = serde_yaml::Mapping::new();
        root.insert("network".into(), Value::Mapping(net));
        // Add a header comment via the serializer — serde_yaml
        // doesn't preserve comments, so we prepend by string
        // concat to ensure the file is recognisable to ops.
        let body = serde_yaml::to_string(&Value::Mapping(root))?;
        Ok(format!(
            "# Managed by nexus-engine via nexus-netd. Do NOT edit by hand —\n\
             # your changes will be overwritten on the next admin apply.\n\
             # Operator-managed netplan files go in a separate /etc/netplan/*.yaml\n\
             # (netplan merges every file in lexical order; ours sorts 90-).\n\
             {body}"
        ))
    }

    /// Validate the plan against a list of physical NIC names
    /// the OS actually has. Catches typos + impossible configs
    /// (VLAN linking to a non-existent NIC, gateway with no
    /// matching subnet, etc.) up-front so the operator sees a
    /// 400 instead of a `netplan apply` failure.
    pub fn validate(&self, known_physical: &[String]) -> Result<(), PlanError> {
        for (name, eth) in &self.ethernets {
            if !known_physical.iter().any(|n| n == name) {
                return Err(PlanError::Invalid(format!(
                    "ethernets.{name}: no such physical interface on this host"
                )));
            }
            validate_eth(name, eth)?;
        }
        for (name, v) in &self.vlans {
            validate_vlan(name, v, &self.ethernets, known_physical)?;
        }
        Ok(())
    }
}

fn validate_eth(name: &str, eth: &EthernetConfig) -> Result<(), PlanError> {
    let static_set = !eth.addresses.is_empty();
    let dhcp_on = eth.dhcp4 == Some(true);
    if static_set && dhcp_on {
        return Err(PlanError::Invalid(format!(
            "ethernets.{name}: addresses + dhcp4=true are mutually exclusive"
        )));
    }
    for a in &eth.addresses {
        validate_cidr(name, a)?;
    }
    if let Some(mtu) = eth.mtu {
        if !(68..=9216).contains(&mtu) {
            return Err(PlanError::Invalid(format!(
                "ethernets.{name}.mtu: {mtu} outside [68, 9216]"
            )));
        }
    }
    if let Some(mac) = &eth.macaddress {
        validate_mac(name, mac)?;
    }
    if let Some(gw) = eth.gateway {
        ensure_gateway_in_subnet(name, gw, &eth.addresses)?;
    }
    Ok(())
}

fn validate_vlan(
    name: &str,
    v: &VlanConfig,
    eths: &BTreeMap<String, EthernetConfig>,
    known_physical: &[String],
) -> Result<(), PlanError> {
    if !(1..=4094).contains(&v.id) {
        return Err(PlanError::Invalid(format!(
            "vlans.{name}.id: {} outside [1, 4094]",
            v.id
        )));
    }
    // The conventional name (`<parent>.<id>`) is also enforced —
    // arbitrary VLAN names make the runtime correlate-by-name
    // logic harder and don't buy operators anything.
    let want = format!("{}.{}", v.link, v.id);
    if name != want {
        return Err(PlanError::Invalid(format!(
            "vlans.{name}: name must follow `<link>.<id>` convention (expected `{want}`)"
        )));
    }
    if !eths.contains_key(&v.link) && !known_physical.iter().any(|n| n == &v.link) {
        return Err(PlanError::Invalid(format!(
            "vlans.{name}.link: parent `{}` not declared in ethernets nor present on host",
            v.link
        )));
    }
    let static_set = !v.addresses.is_empty();
    let dhcp_on = v.dhcp4 == Some(true);
    if static_set && dhcp_on {
        return Err(PlanError::Invalid(format!(
            "vlans.{name}: addresses + dhcp4=true are mutually exclusive"
        )));
    }
    for a in &v.addresses {
        validate_cidr(name, a)?;
    }
    if let Some(mtu) = v.mtu {
        if !(68..=9216).contains(&mtu) {
            return Err(PlanError::Invalid(format!(
                "vlans.{name}.mtu: {mtu} outside [68, 9216]"
            )));
        }
    }
    if let Some(gw) = v.gateway {
        ensure_gateway_in_subnet(name, gw, &v.addresses)?;
    }
    Ok(())
}

fn validate_cidr(scope: &str, addr: &str) -> Result<(), PlanError> {
    let Some((ip, prefix)) = addr.split_once('/') else {
        return Err(PlanError::Invalid(format!(
            "{scope}.addresses: `{addr}` must be CIDR (host/prefix)"
        )));
    };
    let parsed_ip: IpAddr = ip.parse().map_err(|e| {
        PlanError::Invalid(format!("{scope}.addresses: `{addr}` host parse error: {e}"))
    })?;
    let parsed_prefix: u8 = prefix.parse().map_err(|e| {
        PlanError::Invalid(format!(
            "{scope}.addresses: `{addr}` prefix parse error: {e}"
        ))
    })?;
    let max = if matches!(parsed_ip, IpAddr::V4(_)) {
        32
    } else {
        128
    };
    if parsed_prefix == 0 || parsed_prefix > max {
        return Err(PlanError::Invalid(format!(
            "{scope}.addresses: `{addr}` prefix /{parsed_prefix} outside (0, {max}]"
        )));
    }
    Ok(())
}

fn validate_mac(scope: &str, mac: &str) -> Result<(), PlanError> {
    let parts: Vec<&str> = mac.split(':').collect();
    if parts.len() != 6 || !parts.iter().all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit())) {
        return Err(PlanError::Invalid(format!(
            "{scope}.macaddress: `{mac}` must be `aa:bb:cc:dd:ee:ff`"
        )));
    }
    Ok(())
}

fn ensure_gateway_in_subnet(
    scope: &str,
    gw: IpAddr,
    addrs: &[String],
) -> Result<(), PlanError> {
    if addrs.is_empty() {
        // DHCP case — defer to the DHCP server, no constraint here.
        return Ok(());
    }
    // Only check v4 for now (v6 SLAAC + ULA + GUA combinations
    // are messier and we don't expose them in the UI yet).
    let IpAddr::V4(gw_v4) = gw else {
        return Ok(());
    };
    let mut ok = false;
    for a in addrs {
        if let Some((ip, prefix)) = a.split_once('/') {
            if let (Ok(IpAddr::V4(a_v4)), Ok(p)) = (ip.parse::<IpAddr>(), prefix.parse::<u8>()) {
                if v4_in_subnet(gw_v4, a_v4, p) {
                    ok = true;
                    break;
                }
            }
        }
    }
    if !ok {
        return Err(PlanError::Invalid(format!(
            "{scope}.gateway: {gw} is not in any configured subnet"
        )));
    }
    Ok(())
}

fn v4_in_subnet(ip: Ipv4Addr, net: Ipv4Addr, prefix: u8) -> bool {
    if prefix == 0 {
        return true;
    }
    let mask: u32 = u32::MAX
        .checked_shl(32 - prefix as u32)
        .unwrap_or(0);
    (u32::from(ip) & mask) == (u32::from(net) & mask)
}

fn ethernet_to_yaml(eth: &EthernetConfig) -> Result<serde_yaml::Value, PlanError> {
    use serde_yaml::{Mapping, Value};
    let mut m = Mapping::new();
    if let Some(d) = eth.dhcp4 {
        m.insert("dhcp4".into(), d.into());
    }
    if !eth.addresses.is_empty() {
        let v: Vec<Value> = eth.addresses.iter().cloned().map(Into::into).collect();
        m.insert("addresses".into(), Value::Sequence(v));
    }
    if let Some(gw) = eth.gateway {
        let mut route = Mapping::new();
        route.insert("to".into(), "default".into());
        route.insert("via".into(), gw.to_string().into());
        m.insert("routes".into(), Value::Sequence(vec![Value::Mapping(route)]));
    }
    if let Some(ns) = &eth.nameservers {
        m.insert("nameservers".into(), nameservers_to_yaml(ns));
    }
    if let Some(mtu) = eth.mtu {
        m.insert("mtu".into(), (mtu as i64).into());
    }
    if let Some(mac) = &eth.macaddress {
        m.insert("macaddress".into(), mac.clone().into());
    }
    Ok(Value::Mapping(m))
}

fn vlan_to_yaml(v: &VlanConfig) -> Result<serde_yaml::Value, PlanError> {
    use serde_yaml::{Mapping, Value};
    let mut m = Mapping::new();
    m.insert("id".into(), (v.id as i64).into());
    m.insert("link".into(), v.link.clone().into());
    if let Some(d) = v.dhcp4 {
        m.insert("dhcp4".into(), d.into());
    }
    if !v.addresses.is_empty() {
        let xs: Vec<Value> = v.addresses.iter().cloned().map(Into::into).collect();
        m.insert("addresses".into(), Value::Sequence(xs));
    }
    if let Some(gw) = v.gateway {
        let mut route = Mapping::new();
        route.insert("to".into(), "default".into());
        route.insert("via".into(), gw.to_string().into());
        m.insert("routes".into(), Value::Sequence(vec![Value::Mapping(route)]));
    }
    if let Some(ns) = &v.nameservers {
        m.insert("nameservers".into(), nameservers_to_yaml(ns));
    }
    if let Some(mtu) = v.mtu {
        m.insert("mtu".into(), (mtu as i64).into());
    }
    Ok(Value::Mapping(m))
}

fn nameservers_to_yaml(ns: &Nameservers) -> serde_yaml::Value {
    use serde_yaml::{Mapping, Value};
    let mut m = Mapping::new();
    if !ns.addresses.is_empty() {
        let xs: Vec<Value> = ns.addresses.iter().map(|a| a.to_string().into()).collect();
        m.insert("addresses".into(), Value::Sequence(xs));
    }
    if !ns.search.is_empty() {
        let xs: Vec<Value> = ns.search.iter().cloned().map(Into::into).collect();
        m.insert("search".into(), Value::Sequence(xs));
    }
    Value::Mapping(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_nics() -> Vec<String> {
        vec!["eno1".into(), "eno2".into()]
    }

    #[test]
    fn yaml_round_trip_minimal() {
        let mut plan = NetplanPlan::default();
        plan.ethernets.insert(
            "eno1".into(),
            EthernetConfig {
                dhcp4: Some(false),
                addresses: vec!["192.168.1.66/24".into()],
                gateway: Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))),
                ..Default::default()
            },
        );
        let yaml = plan.to_yaml().unwrap();
        assert!(yaml.contains("eno1:"), "missing eno1 key:\n{yaml}");
        assert!(yaml.contains("to: default"), "missing default route:\n{yaml}");
        // Deprecated keys must NOT appear.
        assert!(!yaml.contains("gateway4"));
    }

    #[test]
    fn vlan_must_match_parent_dot_id_name() {
        let mut plan = NetplanPlan::default();
        plan.ethernets.insert("eno1".into(), EthernetConfig::default());
        plan.vlans.insert(
            "secure_vlan".into(),
            VlanConfig {
                id: 20,
                link: "eno1".into(),
                dhcp4: Some(true),
                addresses: vec![],
                gateway: None,
                nameservers: None,
                mtu: None,
            },
        );
        let err = plan.validate(&host_nics()).unwrap_err();
        assert!(format!("{err}").contains("must follow `<link>.<id>` convention"));
    }

    #[test]
    fn dhcp_static_mutex() {
        let mut plan = NetplanPlan::default();
        plan.ethernets.insert(
            "eno1".into(),
            EthernetConfig {
                dhcp4: Some(true),
                addresses: vec!["192.168.1.66/24".into()],
                ..Default::default()
            },
        );
        let err = plan.validate(&host_nics()).unwrap_err();
        assert!(format!("{err}").contains("mutually exclusive"));
    }

    #[test]
    fn vlan_links_to_known_parent() {
        let mut plan = NetplanPlan::default();
        plan.vlans.insert(
            "eno9.10".into(),
            VlanConfig {
                id: 10,
                link: "eno9".into(),
                dhcp4: Some(true),
                addresses: vec![],
                gateway: None,
                nameservers: None,
                mtu: None,
            },
        );
        let err = plan.validate(&host_nics()).unwrap_err();
        assert!(format!("{err}").contains("not declared in ethernets nor present on host"));
    }

    #[test]
    fn gateway_must_be_in_subnet() {
        let mut plan = NetplanPlan::default();
        plan.ethernets.insert(
            "eno1".into(),
            EthernetConfig {
                dhcp4: Some(false),
                addresses: vec!["192.168.1.66/24".into()],
                gateway: Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))),
                ..Default::default()
            },
        );
        let err = plan.validate(&host_nics()).unwrap_err();
        assert!(format!("{err}").contains("not in any configured subnet"));
    }

    #[test]
    fn rejects_unknown_physical_interface() {
        let mut plan = NetplanPlan::default();
        plan.ethernets.insert("eno9".into(), EthernetConfig::default());
        let err = plan.validate(&host_nics()).unwrap_err();
        assert!(format!("{err}").contains("no such physical interface"));
    }

    #[test]
    fn full_vlan_yaml_shape() {
        let mut plan = NetplanPlan::default();
        plan.ethernets.insert(
            "eno1".into(),
            EthernetConfig {
                dhcp4: Some(false),
                addresses: vec!["192.168.1.66/24".into()],
                gateway: Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))),
                ..Default::default()
            },
        );
        plan.vlans.insert(
            "eno1.20".into(),
            VlanConfig {
                id: 20,
                link: "eno1".into(),
                dhcp4: Some(false),
                addresses: vec!["10.0.20.10/24".into()],
                gateway: None,
                nameservers: Some(Nameservers {
                    addresses: vec!["1.1.1.1".parse().unwrap()],
                    search: vec![],
                }),
                mtu: Some(1500),
            },
        );
        plan.validate(&host_nics()).unwrap();
        let yaml = plan.to_yaml().unwrap();
        assert!(yaml.contains("eno1.20:"));
        assert!(yaml.contains("id: 20"));
        assert!(yaml.contains("link: eno1"));
        assert!(yaml.contains("10.0.20.10/24"));
    }

    #[test]
    fn json_round_trip_lossless() {
        let mut plan = NetplanPlan::default();
        plan.ethernets.insert(
            "eno1".into(),
            EthernetConfig {
                dhcp4: Some(false),
                addresses: vec!["192.168.1.66/24".into()],
                gateway: Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))),
                mtu: Some(9000),
                macaddress: Some("aa:bb:cc:dd:ee:ff".into()),
                nameservers: Some(Nameservers {
                    addresses: vec!["1.1.1.1".parse().unwrap(), "8.8.8.8".parse().unwrap()],
                    search: vec!["lan".into()],
                }),
            },
        );
        let j = plan.to_json().unwrap();
        let back = NetplanPlan::from_json(&j).unwrap();
        assert_eq!(plan, back);
    }
}
