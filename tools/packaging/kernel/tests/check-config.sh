#!/usr/bin/env bash
# Copyright (c) 2026 Datadog, Inc.
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

check() {
	awk -f "$(dirname "$0")/../check-config.awk" \
		input=exceptions <(printf '%s\n' '' '# CONFIG_NET_SCH_PRIO' CONFIG_NET_SCH) \
		input=fragments <(printf '%s\n' CONFIG_NET_SCH=y CONFIG_NET_SCH_PRIO=m) \
			<(echo CONFIG_NET_SCH_PRIO=y) input=generated -
}

check <<< 'CONFIG_NET_SCH_PRIO=y'
for actual in '' 'CONFIG_NET_SCH_PRIO=m'; do
	if check <<< "${actual}"; then
		echo "Accepted a missing or incorrect PRIO setting: '${actual}'" >&2
		exit 1
	fi
done
