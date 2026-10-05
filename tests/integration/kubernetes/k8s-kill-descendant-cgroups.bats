#!/usr/bin/env bats
# Copyright (c) 2026 Datadog, Inc.
# SPDX-License-Identifier: Apache-2.0

load "${BATS_TEST_DIRNAME}/lib.sh"
load "${BATS_TEST_DIRNAME}/../../common.bash"
load "${BATS_TEST_DIRNAME}/tests_common.sh"

setup() {
	setup_common || die "setup_common failed"
	pod_name="kill-descendant-cgroups"
	policy_settings_dir=""
	exec_host "${node}" "test -f /sys/fs/cgroup/cgroup.controllers" >/dev/null 2>&1 || \
		skip "test requires cgroup v2"
	yaml_file="${pod_config_dir}/pod-kill-descendant-cgroups.yaml"
	set_node "${yaml_file}" "${node}"
	command='test -z "$(cat /sys/fs/cgroup/cgroup.procs)" && test -n "$(cat /sys/fs/cgroup/init/cgroup.procs)" && test -n "$(cat /sys/fs/cgroup/nested/worker/cgroup.procs)"'
	policy_settings_dir="$(create_tmp_policy_settings_dir "${pod_config_dir}")"
	add_exec_to_policy_settings "${policy_settings_dir}" "sh" "-c" "${command}"
	auto_generate_policy "${policy_settings_dir}" "${yaml_file}"
}

@test "SIGKILL terminates processes below an empty container cgroup" {
	kubectl create -f "${yaml_file}"
	kubectl wait --for=condition=Ready --timeout="${timeout}" pod "${pod_name}"

	# Exec processes join /init as well, so checking the parent remains valid.
	kubectl exec "${pod_name}" -- sh -c \
		"${command}"
	kubectl delete pod "${pod_name}" --wait=false
	kubectl wait --for=delete --timeout=60s pod "${pod_name}"
}

teardown() {
	kubectl delete pod "${pod_name}" --ignore-not-found --wait=false
	delete_tmp_policy_settings_dir "${policy_settings_dir}"
	teardown_common "${node}" "${node_start_time:-}"
}
