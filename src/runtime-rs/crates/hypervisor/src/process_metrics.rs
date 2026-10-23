// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

use anyhow::Result;
use procfs::process::Process;
use prometheus::{Encoder, Gauge, GaugeVec, Opts, Registry, TextEncoder};

pub struct ProcessMetricsHelp {
    pub threads: &'static str,
    pub proc_status: &'static str,
    pub proc_stat: &'static str,
    pub io_stat: &'static str,
    pub fds: &'static str,
    pub netdev: Option<&'static str>,
}

/// Per-process gauges with the names, labels and units of the Go shim's
/// kata_shim, kata_hypervisor and kata_virtiofsd collectors.
pub struct ProcessMetrics {
    threads: Gauge,
    proc_status: GaugeVec,
    proc_stat: GaugeVec,
    io_stat: GaugeVec,
    fds: Gauge,
    netdev: Option<GaugeVec>,
    registry: Registry,
}

impl ProcessMetrics {
    pub fn new(namespace: &str, help: ProcessMetricsHelp) -> Result<Self> {
        let opts = |name: &str, help: &str| Opts::new(name, help).namespace(namespace);
        let metrics = Self {
            threads: Gauge::with_opts(opts("threads", help.threads))?,
            proc_status: GaugeVec::new(opts("proc_status", help.proc_status), &["item"])?,
            proc_stat: GaugeVec::new(opts("proc_stat", help.proc_stat), &["item"])?,
            io_stat: GaugeVec::new(opts("io_stat", help.io_stat), &["item"])?,
            fds: Gauge::with_opts(opts("fds", help.fds))?,
            netdev: help
                .netdev
                .map(|h| GaugeVec::new(opts("netdev", h), &["interface", "item"]))
                .transpose()?,
            registry: Registry::new(),
        };
        metrics.register(&metrics.registry)?;
        Ok(metrics)
    }

    /// Updates the gauges from /proc/<pid> and returns them in text format.
    pub fn text(&self, pid: i32) -> Result<String> {
        self.update(pid)?;
        encode(&self.registry)
    }

    pub fn register(&self, registry: &Registry) -> Result<()> {
        registry.register(Box::new(self.threads.clone()))?;
        registry.register(Box::new(self.proc_status.clone()))?;
        registry.register(Box::new(self.proc_stat.clone()))?;
        if let Some(netdev) = &self.netdev {
            registry.register(Box::new(netdev.clone()))?;
        }
        registry.register(Box::new(self.io_stat.clone()))?;
        registry.register(Box::new(self.fds.clone()))?;
        Ok(())
    }

    /// Updates the gauges from /proc/<pid>; like the Go collector, a part that
    /// cannot be read keeps its previous value.
    pub fn update(&self, pid: i32) -> Result<()> {
        let proc = Process::new(pid)?;

        if let Ok(fds) = proc.fd_count() {
            self.fds.set(fds as f64);
        }
        if let (Some(netdev), Ok(devs)) = (&self.netdev, proc.dev_status()) {
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
                    netdev
                        .with_label_values(&[d.name.as_str(), item])
                        .set(v as f64);
                }
            }
        }
        if let Ok(stat) = proc.stat() {
            self.threads.set(stat.num_threads as f64);
            for (item, v) in [
                ("utime", stat.utime as f64),
                ("stime", stat.stime as f64),
                ("cutime", stat.cutime as f64),
                ("cstime", stat.cstime as f64),
            ] {
                self.proc_stat.with_label_values(&[item]).set(v);
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
                self.proc_status
                    .with_label_values(&[item])
                    .set((kb.unwrap_or(0) * 1024) as f64);
            }
            for (item, v) in [
                ("voluntary_ctxt_switches", s.voluntary_ctxt_switches),
                ("nonvoluntary_ctxt_switches", s.nonvoluntary_ctxt_switches),
            ] {
                self.proc_status
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
                self.io_stat.with_label_values(&[item]).set(v as f64);
            }
        }
        Ok(())
    }
}

pub fn encode(registry: &Registry) -> Result<String> {
    let mut buffer = Vec::new();
    TextEncoder::new().encode(&registry.gather(), &mut buffer)?;
    Ok(String::from_utf8(buffer)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_metrics() {
        let metrics = ProcessMetrics::new(
            "kata_test",
            ProcessMetricsHelp {
                threads: "t",
                proc_status: "s",
                proc_stat: "st",
                io_stat: "io",
                fds: "f",
                netdev: Some("n"),
            },
        )
        .unwrap();
        let out = metrics.text(std::process::id() as i32).unwrap();
        for name in [
            "kata_test_threads",
            "kata_test_fds",
            "kata_test_proc_status{item=\"vmrss\"}",
            "kata_test_proc_stat{item=\"utime\"}",
            "kata_test_io_stat{item=\"readbytes\"}",
            "kata_test_netdev{interface=\"lo\",item=\"recv_bytes\"}",
        ] {
            assert!(out.contains(name), "{} missing from:\n{}", name, out);
        }
    }
}
