// SPDX-License-Identifier: Apache-2.0
//! The PowerView hosts `host = "auto"` tries from WSL2: loopback (mirrored networking), then the
//! default gateway and the resolv.conf nameserver (NAT networking, where Windows is the gateway).

use std::net::Ipv4Addr;

/// Hosts for `auto`, in order, without duplicates: 127.0.0.1, `gateway`, `nameserver`.
pub fn auto_candidates(gateway: Option<&str>, nameserver: Option<&str>) -> Vec<String> {
    let mut v = vec!["127.0.0.1".to_string()];
    for h in [gateway, nameserver].into_iter().flatten() {
        if !v.iter().any(|c| c == h) {
            v.push(h.to_string());
        }
    }
    v
}

/// `auto_candidates` of this machine. Never fails: an unreadable file contributes nothing.
pub fn system_auto_candidates() -> Vec<String> {
    let gateway = read_default_gateway().map(|g| g.to_string());
    auto_candidates(gateway.as_deref(), read_nameserver().as_deref())
}

/// The default gateway from `/proc/net/route`.
pub fn read_default_gateway() -> Option<Ipv4Addr> {
    std::fs::read_to_string("/proc/net/route").ok().and_then(|r| gateway_from_proc_route(&r))
}

/// The first nameserver of `/etc/resolv.conf`.
pub fn read_nameserver() -> Option<String> {
    std::fs::read_to_string("/etc/resolv.conf").ok().and_then(|t| nameserver_from_resolv_conf(&t))
}

/// Default route of `/proc/net/route`: destination `00000000`, gateway as little-endian hex.
pub fn gateway_from_proc_route(route: &str) -> Option<Ipv4Addr> {
    route.lines().skip(1).find_map(|l| {
        let cols: Vec<&str> = l.split_whitespace().collect();
        if cols.get(1) != Some(&"00000000") {
            return None;
        }
        let g = u32::from_str_radix(cols.get(2)?, 16).ok()?;
        (g != 0).then(|| Ipv4Addr::from(g.to_le_bytes()))
    })
}

pub fn nameserver_from_resolv_conf(text: &str) -> Option<String> {
    text.lines().find_map(|l| l.strip_prefix("nameserver ")).map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_gateway_is_decoded_from_little_endian_hex() {
        let t = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                 eth0\t0010A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n\
                 eth0\t00000000\t01201DAC\t0003\t0\t0\t0\t00000000\t0\t0\t0\n";
        assert_eq!(gateway_from_proc_route(t), Some(Ipv4Addr::new(172, 29, 32, 1)));
        assert_eq!(gateway_from_proc_route("Iface\tDestination\n"), None);
    }

    #[test]
    fn auto_hosts_are_loopback_then_gateway_then_nameserver_without_duplicates() {
        assert_eq!(auto_candidates(Some("172.29.32.1"), Some("172.29.32.1")), ["127.0.0.1", "172.29.32.1"]);
        assert_eq!(auto_candidates(None, Some("10.0.0.1")), ["127.0.0.1", "10.0.0.1"]);
        assert_eq!(auto_candidates(Some("127.0.0.1"), None), ["127.0.0.1"]);
        assert_eq!(nameserver_from_resolv_conf("# x\nnameserver 10.0.0.1\n"), Some("10.0.0.1".into()));
    }
}
