//! Session-scoped DNS routing addresses, never sent to an upstream as IP targets.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex};

const V4: Ipv4Addr = Ipv4Addr::new(198, 19, 0, 0);
const V6: Ipv6Addr = Ipv6Addr::new(0xfd23, 0x656e, 0x7677, 1, 0, 0, 0, 0);
const LIMIT: usize = 65_534;

#[derive(Default)]
struct Entries {
    indices: HashMap<Arc<str>, u16>,
    names: Vec<Arc<str>>,
}
#[derive(Default)]
pub struct Domains(Mutex<Entries>);

pub fn reserved(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(ip) => u32::from(ip) >> 16 == u32::from(V4) >> 16,
        IpAddr::V6(ip) => u128::from(ip) >> 64 == u128::from(V6) >> 64,
    }
}
pub fn overlaps(range: &ipnet::IpNet) -> bool {
    reserved(range.network()) || range.contains(&IpAddr::V4(V4)) || range.contains(&IpAddr::V6(V6))
}
impl Domains {
    pub fn address(&self, name: &str, ipv6: bool) -> io::Result<IpAddr> {
        let mut entries = self
            .0
            .lock()
            .map_err(|_| io::Error::other("domain table unavailable"))?;
        let index = if let Some(&index) = entries.indices.get(name) {
            index
        } else {
            // Never evict or reuse: an application's cached address must not be
            // redirected to a different domain later in the same session.
            if entries.names.len() == LIMIT {
                return Err(io::Error::other("session domain table is full"));
            }
            let index = (entries.names.len() + 1) as u16;
            let name: Arc<str> = name.into();
            entries.names.push(Arc::clone(&name));
            entries.indices.insert(name, index);
            index
        };
        Ok(if ipv6 {
            IpAddr::V6(Ipv6Addr::from(u128::from(V6) + u128::from(index)))
        } else {
            IpAddr::V4(Ipv4Addr::from(u32::from(V4) + u32::from(index)))
        })
    }
    pub fn lookup(&self, ip: IpAddr) -> io::Result<Arc<str>> {
        let index = match ip.to_canonical() {
            IpAddr::V4(ip) if reserved(ip.into()) => u128::from(u32::from(ip) - u32::from(V4)),
            IpAddr::V6(ip) if reserved(ip.into()) => u128::from(ip) - u128::from(V6),
            _ => return Err(io::Error::other("not a domain routing address")),
        };
        let entries = self
            .0
            .lock()
            .map_err(|_| io::Error::other("domain table unavailable"))?;
        if index == 0 || index > entries.names.len() as u128 {
            return Err(io::Error::other(
                "unknown or expired domain routing address",
            ));
        }
        Ok(Arc::clone(&entries.names[index as usize - 1]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_real_ip_cannot_alias_distinct_domains_and_families_agree() {
        let table = Domains::default();
        let first = table.address("one.example", false).unwrap();
        let second = table.address("two.example", false).unwrap();
        assert_ne!(first, second);
        assert_eq!(&*table.lookup(first).unwrap(), "one.example");
        assert_eq!(&*table.lookup(second).unwrap(), "two.example");
        let v6 = table.address("one.example", true).unwrap();
        assert_eq!(&*table.lookup(v6).unwrap(), "one.example");
        assert_eq!(first, table.address("one.example", false).unwrap());
        assert!(Domains::default().lookup(first).is_err());
    }
    #[test]
    fn exhausted_table_keeps_existing_targets_and_rejects_unallocated_addresses() {
        let table = Domains::default();
        for index in 0..LIMIT {
            table.address(&format!("{index}.example"), false).unwrap();
        }
        assert!(table.address("overflow.example", true).is_err());
        let first = table.address("0.example", false).unwrap();
        assert_eq!(&*table.lookup(first).unwrap(), "0.example");
        for ip in [
            "198.19.0.0",
            "198.19.255.255",
            "fd23:656e:7677:1::1:0",
            "127.0.0.1",
        ] {
            assert!(table.lookup(ip.parse().unwrap()).is_err());
        }
    }
}
