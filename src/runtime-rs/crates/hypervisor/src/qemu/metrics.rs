// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

use anyhow::Result;
use lazy_static::lazy_static;
use procfs::process::Process;
use prometheus::{Encoder, Gauge, GaugeVec, Opts, Registry, TextEncoder};

const NAMESPACE: &str = "kata_hypervisor";

lazy_static! {
    static ref THREADS: Gauge =
        Gauge::with_opts(Opts::new("threads", "Hypervisor process threads.").namespace(NAMESPACE))
            .unwrap();
    static ref PROC_STATUS: GaugeVec =
        gauge_vec("proc_status", "Hypervisor process status.", &["item"]);
    static ref PROC_STAT: GaugeVec =
        gauge_vec("proc_stat", "Hypervisor process statistics.", &["item"]);
    static ref NETDEV: GaugeVec =
        gauge_vec("netdev", "Net devices statistics.", &["interface", "item"]);
    static ref IO_STAT: GaugeVec = gauge_vec("io_stat", "Process IO statistics.", &["item"]);
    static ref OPEN_FDS: Gauge =
        Gauge::with_opts(Opts::new("fds", "Open FDs for hypervisor.").namespace(NAMESPACE))
            .unwrap();
    static ref REGISTRY: Registry = {
        let registry = Registry::new();
        registry.register(Box::new(THREADS.clone())).unwrap();
        registry.register(Box::new(PROC_STATUS.clone())).unwrap();
        registry.register(Box::new(PROC_STAT.clone())).unwrap();
        registry.register(Box::new(NETDEV.clone())).unwrap();
        registry.register(Box::new(IO_STAT.clone())).unwrap();
        registry.register(Box::new(OPEN_FDS.clone())).unwrap();
        registry
    };
}

fn gauge_vec(name: &str, help: &str, labels: &[&str]) -> GaugeVec {
    GaugeVec::new(Opts::new(name, help).namespace(NAMESPACE), labels).unwrap()
}

/// Same metrics, names and units as the Go shim's kata_hypervisor_* collector.
pub(crate) fn hypervisor_metrics(pid: u32) -> Result<String> {
    let proc = Process::new(pid as i32)?;

    if let Ok(fds) = proc.fd_count() {
        OPEN_FDS.set(fds as f64);
    }
    if let Ok(devs) = proc.dev_status() {
        for d in devs.values() {
            for (item, v) in [
                ("recv_bytes", d.recv_bytes),
                ("recv_packets", d.recv_packets),
                ("recv_errs", d.recv_errs),
                ("recv_drop", d.recv_drop),
                ("recv_compressed", d.recv_compressed),
                ("recv_fifo", d.recv_fifo),
                ("recv_frame", d.recv_frame),
                ("recv_multicast", d.recv_multicast),
                ("sent_bytes", d.sent_bytes),
                ("sent_packets", d.sent_packets),
                ("sent_errs", d.sent_errs),
                ("sent_drop", d.sent_drop),
                ("sent_colls", d.sent_colls),
                ("sent_carrier", d.sent_carrier),
                ("sent_compressed", d.sent_compressed),
                ("sent_fifo", d.sent_fifo),
            ] {
                NETDEV
                    .with_label_values(&[d.name.as_str(), item])
                    .set(v as f64);
            }
        }
    }
    if let Ok(stat) = proc.stat() {
        THREADS.set(stat.num_threads as f64);
        for (item, v) in [
            ("utime", stat.utime as f64),
            ("stime", stat.stime as f64),
            ("cutime", stat.cutime as f64),
            ("cstime", stat.cstime as f64),
        ] {
            PROC_STAT.with_label_values(&[item]).set(v);
        }
    }
    if let Ok(s) = proc.status() {
        // procfs reports these in kB, the Go collector in bytes.
        for (item, kb) in [
            ("vmpeak", s.vmpeak),
            ("vmsize", s.vmsize),
            ("vmlck", s.vmlck),
            ("vmpin", s.vmpin),
            ("vmhwm", s.vmhwm),
            ("vmrss", s.vmrss),
            ("rssanon", s.rssanon),
            ("rssfile", s.rssfile),
            ("rssshmem", s.rssshmem),
            ("vmdata", s.vmdata),
            ("vmstk", s.vmstk),
            ("vmexe", s.vmexe),
            ("vmlib", s.vmlib),
            ("vmpte", s.vmpte),
            ("vmswap", s.vmswap),
            ("hugetlbpages", s.hugetlbpages),
        ] {
            PROC_STATUS
                .with_label_values(&[item])
                .set((kb.unwrap_or(0) * 1024) as f64);
        }
        for (item, v) in [
            ("voluntary_ctxt_switches", s.voluntary_ctxt_switches),
            ("nonvoluntary_ctxt_switches", s.nonvoluntary_ctxt_switches),
        ] {
            PROC_STATUS
                .with_label_values(&[item])
                .set(v.unwrap_or(0) as f64);
        }
    }
    if let Ok(io) = proc.io() {
        for (item, v) in [
            ("rchar", io.rchar),
            ("wchar", io.wchar),
            ("syscr", io.syscr),
            ("syscw", io.syscw),
            ("readbytes", io.read_bytes),
            ("writebytes", io.write_bytes),
            ("cancelledwritebytes", io.cancelled_write_bytes),
        ] {
            IO_STAT.with_label_values(&[item]).set(v as f64);
        }
    }

    let mut buffer = Vec::new();
    TextEncoder::new().encode(&REGISTRY.gather(), &mut buffer)?;
    Ok(String::from_utf8(buffer)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hypervisor_metrics() {
        let out = hypervisor_metrics(std::process::id()).unwrap();
        for name in [
            "kata_hypervisor_threads",
            "kata_hypervisor_fds",
            "kata_hypervisor_proc_status{item=\"vmrss\"}",
            "kata_hypervisor_proc_stat{item=\"utime\"}",
            "kata_hypervisor_io_stat{item=\"readbytes\"}",
            "kata_hypervisor_netdev{interface=\"lo\",item=\"recv_bytes\"}",
        ] {
            assert!(out.contains(name), "{} missing from:\n{}", name, out);
        }
    }
}
