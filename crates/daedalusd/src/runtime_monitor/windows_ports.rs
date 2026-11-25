use crate::process::inventory::descendant_pids_including_roots;
use daedalus_proto::{PortMapping, PortSource};
use std::collections::BTreeSet;
use std::mem::{offset_of, size_of};
use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID,
    MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};
pub fn discover_local_ports_for_roots(roots: &[u32]) -> Vec<PortMapping> {
    let pids = descendant_pids_including_roots(roots);
    let mut ports = BTreeSet::new();
    if !pids.is_empty() {
        for family in [AF_INET, AF_INET6] {
            let Some(table) = listener_table(family) else {
                continue;
            };
            let Some(&count) = table.first() else {
                continue;
            };
            // SDK offsets include any padding between the count and the first row.
            // The port and owner fields in both families are DWORDs.
            let (start, row_size, owner, local_port) = if family == AF_INET {
                (
                    offset_of!(MIB_TCPTABLE_OWNER_PID, table),
                    size_of::<MIB_TCPROW_OWNER_PID>(),
                    offset_of!(MIB_TCPROW_OWNER_PID, dwOwningPid),
                    offset_of!(MIB_TCPROW_OWNER_PID, dwLocalPort),
                )
            } else {
                (
                    offset_of!(MIB_TCP6TABLE_OWNER_PID, table),
                    size_of::<MIB_TCP6ROW_OWNER_PID>(),
                    offset_of!(MIB_TCP6ROW_OWNER_PID, dwOwningPid),
                    offset_of!(MIB_TCP6ROW_OWNER_PID, dwLocalPort),
                )
            };
            let word = size_of::<u32>();
            let Some(rows) = table.get(start / word..) else {
                continue;
            };
            for row in rows.chunks_exact(row_size / word).take(count as usize) {
                let pid = row[owner / word];
                let port = u16::from_be(row[local_port / word] as u16);
                if port != 0 && pids.contains(&pid) {
                    ports.insert(port);
                }
            }
        }
    }
    ports
        .into_iter()
        .map(|host_port| PortMapping {
            host_port,
            container_port: None,
            service: None,
            source: PortSource::LocalProcess,
        })
        .collect()
}

fn listener_table(family: u16) -> Option<Vec<u32>> {
    // Both IP Helper table layouts are DWORD-aligned. A table can grow between calls.
    let mut data = vec![0u32; 256];
    for _ in 0..5 {
        let mut bytes = u32::try_from(std::mem::size_of_val(data.as_slice())).ok()?;
        // SAFETY: data has space for bytes writable bytes and IP Helper does not retain it.
        let status = unsafe {
            GetExtendedTcpTable(
                data.as_mut_ptr().cast(),
                &mut bytes,
                0,
                u32::from(family),
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        match status {
            ERROR_SUCCESS => return Some(data),
            ERROR_INSUFFICIENT_BUFFER => {
                data.resize((bytes as usize).div_ceil(size_of::<u32>()), 0)
            }
            _ => return None,
        }
    }
    None
}
