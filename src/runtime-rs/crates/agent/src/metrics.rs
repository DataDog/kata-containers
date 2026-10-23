// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

use std::sync::LazyLock;

use prometheus::{exponential_buckets, HistogramOpts, HistogramVec};

pub static AGENT_RPC_DURATIONS: LazyLock<HistogramVec> = LazyLock::new(|| {
    HistogramVec::new(
        HistogramOpts::new(
            "agent_rpc_durations_histogram_milliseconds",
            "RPC latency distributions.",
        )
        .namespace("kata_shim")
        .buckets(exponential_buckets(1.0, 2.0, 10).unwrap()),
        &["action"],
    )
    .unwrap()
});
