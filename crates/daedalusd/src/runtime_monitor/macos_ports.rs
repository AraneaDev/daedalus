use daedalus_proto::{PortMapping, PortSource};
use libproc::libproc::bsd_info::BSDInfo;
use libproc::libproc::file_info::{ListFDs, ProcFDType, pidfdinfo};
use libproc::libproc::net_info::{SocketFDInfo, SocketInfoKind, TcpSIState};
use libproc::libproc::proc_pid::{listpidinfo, name, pidinfo};
use libproc::processes::{ProcFilter, pids_by_type};
use std::collections::{BTreeMap, HashSet};

pub fn collect_descendant_pids(roots: &[u32]) -> HashSet<u32> {
    let mut pending = roots.to_vec();
    let mut seen = HashSet::new();
    while let Some(pid) = pending.pop() {
        if pid != 0
            && seen.insert(pid)
            && let Ok(children) = pids_by_type(ProcFilter::ByParentProcess { ppid: pid })
        {
            pending.extend(children);
        }
    }
    seen
}

pub fn discover_local_ports_for_roots(roots: &[u32]) -> Vec<PortMapping> {
    let mut by_port = BTreeMap::new();
    let mut pids: Vec<_> = collect_descendant_pids(roots).into_iter().collect();
    pids.sort_unstable();
    for pid in pids {
        let Ok(pid) = i32::try_from(pid) else {
            continue;
        };
        let Ok(process) = pidinfo::<BSDInfo>(pid, 0) else {
            continue;
        };
        let Ok(fds) = listpidinfo::<ListFDs>(pid, process.pbi_nfiles as usize) else {
            continue;
        };
        let mut service = None;
        for fd in fds {
            if !matches!(ProcFDType::from(fd.proc_fdtype), ProcFDType::Socket) {
                continue;
            }
            let Ok(socket) = pidfdinfo::<SocketFDInfo>(pid, fd.proc_fd) else {
                continue;
            };
            if !matches!(
                SocketInfoKind::from(socket.psi.soi_kind),
                SocketInfoKind::Tcp
            ) {
                continue;
            }
            // SAFETY: soi_kind identifies the TCP member of the protocol union.
            let tcp = unsafe { socket.psi.soi_proto.pri_tcp };
            let port = u16::from_be(tcp.tcpsi_ini.insi_lport as u16);
            if port != 0 && matches!(TcpSIState::from(tcp.tcpsi_state), TcpSIState::Listen) {
                let label = service.get_or_insert_with(|| name(pid).ok());
                let existing = by_port.entry(port).or_insert(None);
                if existing.is_none() {
                    existing.clone_from(label);
                }
            }
        }
    }
    by_port
        .into_iter()
        .map(|(host_port, service)| PortMapping {
            host_port,
            container_port: None,
            service,
            source: PortSource::LocalProcess,
        })
        .collect()
}
