// Copyright 2021-2022 Ant Group
//
// SPDX-License-Identifier: Apache-2.0
//

use std::sync::LazyLock;
use std::time::Instant;

use anyhow::Result;
use hypervisor::process_metrics::{encode, ProcessMetrics, ProcessMetricsHelp};
use prometheus::process_collector::ProcessCollector;
use prometheus::{exponential_buckets, Gauge, HistogramOpts, HistogramVec, Opts, Registry};

const NAMESPACE_KATA_SHIM: &str = "kata_shim";

static SHIM_PROCESS: LazyLock<ProcessMetrics> = LazyLock::new(|| {
    ProcessMetrics::new(
        NAMESPACE_KATA_SHIM,
        ProcessMetricsHelp {
            threads: "Kata containerd shim v2 process threads.",
            proc_status: "Kata containerd shim v2 process status.",
            proc_stat: "Kata containerd shim v2 process statistics.",
            io_stat: "Kata containerd shim v2 process IO statistics.",
            fds: "Kata containerd shim v2 open FDs.",
            netdev: None,
        },
    )
    .unwrap()
});

static RPC_DURATIONS: LazyLock<HistogramVec> = LazyLock::new(|| {
    HistogramVec::new(
        HistogramOpts::new(
            "rpc_durations_histogram_milliseconds",
            "RPC latency distributions.",
        )
        .namespace(NAMESPACE_KATA_SHIM)
        .buckets(exponential_buckets(1.0, 2.0, 10).unwrap()),
        &["action"],
    )
    .unwrap()
});

static POD_OVERHEAD_CPU: LazyLock<Gauge> = LazyLock::new(|| {
    Gauge::with_opts(
        Opts::new(
            "pod_overhead_cpu",
            "Kata Pod overhead for CPU resources(percent).",
        )
        .namespace(NAMESPACE_KATA_SHIM),
    )
    .unwrap()
});

static POD_OVERHEAD_MEMORY: LazyLock<Gauge> = LazyLock::new(|| {
    Gauge::with_opts(
        Opts::new(
            "pod_overhead_memory_in_bytes",
            "Kata Pod overhead for memory resources(bytes).",
        )
        .namespace(NAMESPACE_KATA_SHIM),
    )
    .unwrap()
});

static REGISTRY: LazyLock<Registry> = LazyLock::new(|| {
    let registry = Registry::new();
    SHIM_PROCESS.register(&registry).unwrap();
    registry.register(Box::new(RPC_DURATIONS.clone())).unwrap();
    registry
        .register(Box::new(agent::metrics::AGENT_RPC_DURATIONS.clone()))
        .unwrap();
    registry
        .register(Box::new(POD_OVERHEAD_CPU.clone()))
        .unwrap();
    registry
        .register(Box::new(POD_OVERHEAD_MEMORY.clone()))
        .unwrap();
    registry
        .register(Box::new(ProcessCollector::for_self()))
        .unwrap();
    registry
});

pub fn get_shim_metrics() -> Result<String> {
    SHIM_PROCESS.update(std::process::id() as i32)?;
    encode(&REGISTRY)
}

pub fn observe_shim_rpc(action: &str, start: Instant) {
    RPC_DURATIONS
        .with_label_values(&[action])
        .observe(start.elapsed().as_millis() as f64);
}

pub(crate) fn set_pod_overhead(memory: f64, cpu: f64) {
    POD_OVERHEAD_MEMORY.set(memory);
    POD_OVERHEAD_CPU.set(cpu);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shim_metrics_match_go_shim() {
        observe_shim_rpc("create", Instant::now());
        agent::metrics::AGENT_RPC_DURATIONS
            .with_label_values(&["grpc.CheckRequest"])
            .observe(1.0);
        let out = get_shim_metrics().unwrap();
        for name in [
            "kata_shim_threads",
            "kata_shim_fds",
            "kata_shim_proc_status{item=\"vmrss\"}",
            "kata_shim_proc_stat{item=\"utime\"}",
            "kata_shim_io_stat{item=\"readbytes\"}",
            "kata_shim_rpc_durations_histogram_milliseconds_bucket{action=\"create\",le=\"512\"}",
            "kata_shim_agent_rpc_durations_histogram_milliseconds_bucket{action=\"grpc.CheckRequest\",le=\"1\"}",
            "kata_shim_pod_overhead_cpu",
            "kata_shim_pod_overhead_memory_in_bytes",
            "process_cpu_seconds_total",
            "process_resident_memory_bytes",
        ] {
            assert!(out.contains(name), "{} missing from:\n{}", name, out);
        }
    }
}
