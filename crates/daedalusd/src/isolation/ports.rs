use std::collections::{BTreeSet, HashSet};
use std::io::{self, ErrorKind};
use std::net::TcpListener;

const PORT_BLOCK_SIZE: u16 = 100;
const MAX_PROBE_BLOCKS: u16 = 50;

pub(super) fn allocate_port_offset(
    slot_index: u16,
    base_ports: &BTreeSet<u16>,
    reserved_ports: &HashSet<u16>,
) -> io::Result<u16> {
    if slot_index == 0 {
        return Ok(0);
    }

    let mut offset = match slot_index.checked_mul(PORT_BLOCK_SIZE) {
        Some(o) => o,
        None => {
            return Err(io::Error::new(
                ErrorKind::AddrInUse,
                format!("slot_index {slot_index} overflows u16 port offset"),
            ));
        }
    };

    if base_ports.is_empty() {
        return Ok(offset);
    }

    for _ in 0..MAX_PROBE_BLOCKS {
        let block_ok = base_ports.iter().all(|&base| {
            base.checked_add(offset)
                .filter(|&p| p > 0)
                .is_some_and(|p| !reserved_ports.contains(&p) && is_port_available(p))
        });
        if block_ok {
            return Ok(offset);
        }
        offset = match offset.checked_add(PORT_BLOCK_SIZE) {
            Some(next) => next,
            None => break,
        };
    }

    Err(io::Error::new(
        ErrorKind::AddrInUse,
        format!(
            "no conflict-free port block found within {MAX_PROBE_BLOCKS} blocks from slot {slot_index}"
        ),
    ))
}

fn is_port_available(port: u16) -> bool {
    let Ok(v4_listener) = TcpListener::bind(("127.0.0.1", port)) else {
        return false;
    };
    let available = match TcpListener::bind(("::1", port)) {
        Ok(_v6_listener) => true,
        Err(err) => matches!(
            err.kind(),
            ErrorKind::AddrNotAvailable | ErrorKind::Unsupported
        ),
    };
    drop(v4_listener);
    available
}
