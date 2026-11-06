// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

use anyhow::Result;
use lazy_static::lazy_static;

use crate::process_metrics::{ProcessMetrics, ProcessMetricsHelp};

lazy_static! {
    static ref METRICS: ProcessMetrics = ProcessMetrics::new(
        "kata_hypervisor",
        ProcessMetricsHelp {
            threads: "Hypervisor process threads.",
            proc_status: "Hypervisor process status.",
            proc_stat: "Hypervisor process statistics.",
            io_stat: "Process IO statistics.",
            fds: "Open FDs for hypervisor.",
            netdev: Some("Net devices statistics."),
        },
    )
    .unwrap();
}

pub(crate) fn hypervisor_metrics(pid: u32) -> Result<String> {
    METRICS.text(pid as i32)
}
