// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::LazyLock;

use anyhow::Result;
use hypervisor::process_metrics::{ProcessMetrics, ProcessMetricsHelp};

static VIRTIOFSD_PID: AtomicU32 = AtomicU32::new(0);

static VIRTIOFSD_METRICS: LazyLock<ProcessMetrics> = LazyLock::new(|| {
    ProcessMetrics::new(
        "kata_virtiofsd",
        ProcessMetricsHelp {
            threads: "Virtiofsd process threads.",
            proc_status: "Virtiofsd process status.",
            proc_stat: "Virtiofsd process statistics.",
            io_stat: "Process IO statistics.",
            fds: "Open FDs for virtiofsd.",
            netdev: None,
        },
    )
    .unwrap()
});

pub(crate) fn set_virtiofsd_pid(pid: u32) {
    VIRTIOFSD_PID.store(pid, Ordering::Relaxed);
}

/// kata_virtiofsd_* metrics of the virtiofsd started by this shim, empty if none.
pub fn virtiofsd_metrics() -> Result<String> {
    match VIRTIOFSD_PID.load(Ordering::Relaxed) {
        0 => Ok(String::new()),
        pid => VIRTIOFSD_METRICS.text(pid as i32),
    }
}
