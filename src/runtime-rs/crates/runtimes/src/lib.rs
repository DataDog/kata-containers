// Copyright (c) 2019-2022 Alibaba Cloud
// Copyright (c) 2019-2022 Ant Group
//
// SPDX-License-Identifier: Apache-2.0
//

#[macro_use(lazy_static)]
extern crate lazy_static;

#[macro_use]
extern crate slog;

logging::logger_with_subsystem!(sl, "runtimes");

pub mod manager;
pub use manager::{CleanupOutcome, RuntimeHandlerManager};
pub use shim_interface;
pub mod kata_shim_metrics;
mod pod_overhead;
#[allow(dead_code)]
mod shim_metrics;
mod shim_mgmt;
pub mod tracer;
