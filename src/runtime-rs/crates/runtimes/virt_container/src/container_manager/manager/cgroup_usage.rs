// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

use anyhow::Result;

use super::VirtContainerManager;

impl VirtContainerManager {
    /// Sum of the containers' guest cgroup CPU time (ns) and memory usage (bytes).
    pub(super) async fn sum_containers_cgroup_usage(&self) -> Result<(u64, u64)> {
        let (mut cpu, mut memory) = (0, 0);
        for c in self.containers.read().await.values() {
            if let Some(cgroup) = c.stats().await?.and_then(|s| s.cgroup_stats) {
                cpu += cgroup
                    .cpu_stats
                    .and_then(|s| s.cpu_usage)
                    .map_or(0, |u| u.total_usage);
                memory += cgroup
                    .memory_stats
                    .and_then(|s| s.usage)
                    .map_or(0, |u| u.usage);
            }
        }
        Ok((cpu, memory))
    }
}
