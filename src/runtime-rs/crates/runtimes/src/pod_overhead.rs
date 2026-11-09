// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use common::{ContainerManager, Sandbox};

use crate::shim_metrics::set_pod_overhead;

static CONTAINER_MANAGER: OnceLock<Arc<dyn ContainerManager>> = OnceLock::new();
static SAMPLING: AtomicBool = AtomicBool::new(false);

pub(crate) fn init(container_manager: Arc<dyn ContainerManager>) {
    let _ = CONTAINER_MANAGER.set(container_manager);
}

/// As in the Go shim, the overhead is sampled in the background over one
/// second, so a scrape reports the previous sample.
pub(crate) fn spawn_sample(sandbox: Arc<dyn Sandbox>) {
    let Some(container_manager) = CONTAINER_MANAGER.get().cloned() else {
        return;
    };
    if SAMPLING.swap(true, Ordering::AcqRel) {
        return;
    }
    tokio::spawn(async move {
        if let Ok((memory, cpu)) = sample(sandbox.as_ref(), container_manager.as_ref()).await {
            set_pod_overhead(memory, cpu);
        }
        SAMPLING.store(false, Ordering::Release);
    });
}

async fn sample(sandbox: &dyn Sandbox, containers: &dyn ContainerManager) -> Result<(f64, f64)> {
    let start = Instant::now();
    let (host_cpu_start, _) = sandbox.host_cgroup_usage().await?;
    let (guest_cpu_start, _) = containers.containers_cgroup_usage().await?;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let elapsed = start.elapsed().as_nanos() as f64;
    let (host_cpu, host_memory) = sandbox.host_cgroup_usage().await?;
    let (guest_cpu, guest_memory) = containers.containers_cgroup_usage().await?;

    let percent = |end: u64, begin: u64| end.saturating_sub(begin) as f64 / elapsed * 100.0;
    Ok((
        host_memory as f64 - guest_memory as f64,
        percent(host_cpu, host_cpu_start) - percent(guest_cpu, guest_cpu_start),
    ))
}
