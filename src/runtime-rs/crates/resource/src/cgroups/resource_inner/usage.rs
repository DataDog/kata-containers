// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};

use super::CgroupsResourceInner;

impl CgroupsResourceInner {
    /// CPU time (ns) and memory usage (bytes) of the cgroup v2 sandbox cgroup.
    pub(crate) fn sandbox_usage(&self) -> Result<(u64, u64)> {
        if !self.sandbox_cgroup.v2() {
            return Err(anyhow!("sandbox cgroup usage needs cgroup v2"));
        }
        let dir = PathBuf::from(self.sandbox_cgroup.cgroup_path(None)?);
        let cpu_stat = fs::read_to_string(dir.join("cpu.stat")).context("read cpu.stat")?;
        let usage_usec = cpu_stat
            .lines()
            .find_map(|l| l.strip_prefix("usage_usec "))
            .ok_or_else(|| anyhow!("no usage_usec in cpu.stat"))?
            .trim()
            .parse::<u64>()?;
        let memory = fs::read_to_string(dir.join("memory.current"))
            .context("read memory.current")?
            .trim()
            .parse::<u64>()?;
        Ok((usage_usec * 1000, memory))
    }
}
