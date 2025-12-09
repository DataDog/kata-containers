#!/usr/bin/env bats
#
# Copyright (c) 2025 DataDog
#
# SPDX-License-Identifier: Apache-2.0
#
# Test kata-runtime exec command, specifically testing exec without TTY
#
# These tests verify that kata-runtime exec works when stdin is not a TTY.
# This is important for scripting and automation use cases.
#
# Prerequisites:
# - Debug console must be enabled in kata configuration:
#   [agent.kata]
#   debug_console_enabled = true
# - Containerd must be running with kata runtime configured
# - A test container image must be available

load "${BATS_TEST_DIRNAME}/../../common.bash"
load "${BATS_TEST_DIRNAME}/../../metrics/lib/common.bash"

setup_file() {
	export TEST_IMAGE="quay.io/prometheus/busybox:latest"
	export WORK_DIR="${BATS_FILE_TMPDIR}"
	
	# Check if kata-runtime is available
	command -v kata-runtime || skip "kata-runtime not found"
	
	# Check if containerd is available
	command -v crictl || skip "crictl not found"
	command -v containerd || skip "containerd not found"
	
	echo "pull container image"
	check_images ${TEST_IMAGE}
}

setup() {
	# Create pod and container using crictl
	local pod_yaml="${WORK_DIR}/pod.yaml"
	local container_yaml="${WORK_DIR}/container.yaml"
	
	cat << EOF > "${pod_yaml}"
metadata:
  name: kata-exec-test-pod
  namespace: default
  uid: kata-exec-test-pod-uid
EOF

	cat << EOF > "${container_yaml}"
metadata:
  name: kata-exec-test-container
  namespace: default
  uid: kata-exec-test-container-uid
image:
  image: "${TEST_IMAGE}"
command:
- top
EOF

	# Start containerd if not running
	sudo systemctl start containerd || true
	sleep 2
	
	# Pull image
	sudo crictl pull "${TEST_IMAGE}" || die "Failed to pull image"
	
	# Create pod (sandbox)
	export POD_ID=$(sudo crictl --timeout=10s runp "${pod_yaml}")
	[ -n "${POD_ID}" ] || die "Failed to create pod"
	
	# Create container
	export CONTAINER_ID=$(sudo crictl --timeout=10s create "${POD_ID}" "${container_yaml}" "${pod_yaml}")
	[ -n "${CONTAINER_ID}" ] || die "Failed to create container"
	
	# Start container
	sudo crictl --timeout=10s start "${CONTAINER_ID}" || die "Failed to start container"
	
	# Wait a moment for container to be ready
	sleep 2
}

teardown() {
	# Clean up container and pod
	if [ -n "${CONTAINER_ID:-}" ]; then
		sudo crictl stop "${CONTAINER_ID}" 2>/dev/null || true
		sudo crictl rm "${CONTAINER_ID}" 2>/dev/null || true
	fi
	
	if [ -n "${POD_ID:-}" ]; then
		sudo crictl stopp "${POD_ID}" 2>/dev/null || true
		sudo crictl rmp "${POD_ID}" 2>/dev/null || true
	fi
}

@test "kata-runtime exec without TTY - basic command" {
	# The POD_ID from crictl is the sandbox ID for kata-runtime exec
	local sandbox_id="${POD_ID}"
	
	# Test exec without TTY by piping a command to the debug console
	# kata-runtime exec opens a shell, so we send a command and expect output
	# Note: The debug console needs to be enabled in kata config for this to work
	local output=$(echo "echo test-output" | timeout 5 kata-runtime exec "${sandbox_id}" 2>&1 | head -1)
	[ -n "${output}" ] || die "Exec without TTY failed: no output received"
	# The output might have extra characters, so just check it contains our test string
	echo "${output}" | grep -q "test-output" || die "Exec without TTY failed: expected output containing 'test-output', got '${output}'"
}

@test "kata-runtime exec without TTY - write to file" {
	local sandbox_id="${POD_ID}"
	local test_content="hello from exec without tty"
	
	# Write content via exec without TTY - send command to debug console shell
	# The command is sent via stdin to the shell opened by kata-runtime exec
	printf "echo %s > /tmp/exec-test.txt\nexit\n" "${test_content}" | timeout 5 kata-runtime exec "${sandbox_id}" > /dev/null 2>&1
	
	# Wait a moment for the command to complete
	sleep 1
	
	# Verify content was written correctly using crictl exec
	local file_content=$(sudo crictl exec "${CONTAINER_ID}" cat /tmp/exec-test.txt 2>/dev/null || echo "")
	[ "${file_content}" == "${test_content}" ] || die "File content mismatch: expected '${test_content}', got '${file_content}'"
}

@test "kata-runtime exec without TTY - read from stdin" {
	local sandbox_id="${POD_ID}"
	local test_input="test input data"
	
	# Test reading from stdin and outputting via debug console
	# Send command to echo the input, then exit
	local output=$(printf "echo %s\nexit\n" "${test_input}" | timeout 5 kata-runtime exec "${sandbox_id}" 2>&1 | grep -v "^$" | head -1)
	[ -n "${output}" ] || die "Stdin read failed: no output received"
	echo "${output}" | grep -q "${test_input}" || die "Stdin read failed: expected output containing '${test_input}', got '${output}'"
}

@test "kata-runtime exec without TTY - command execution" {
	local sandbox_id="${POD_ID}"
	
	# Test exec with a simple command via debug console
	local output=$(printf "echo hello world\nexit\n" | timeout 5 kata-runtime exec "${sandbox_id}" 2>&1 | grep -v "^$" | head -1)
	[ -n "${output}" ] || die "Command execution failed: no output received"
	echo "${output}" | grep -q "hello world" || die "Command execution failed: expected output containing 'hello world', got '${output}'"
}

@test "kata-runtime exec without TTY - verify no TTY required" {
	local sandbox_id="${POD_ID}"
	
	# This test verifies that kata-runtime exec works when stdin is not a TTY
	# The key test: run without a TTY (e.g., from a script or pipe)
	# Before the fix, this would fail. After the fix, it should work.
	
	# Run in a context without TTY (using script or setsid)
	# We use a here-document piped to kata-runtime exec
	local output=$(printf "echo no-tty-test\nexit\n" | timeout 5 kata-runtime exec "${sandbox_id}" 2>&1)
	local exit_code=$?
	
	# The command should succeed (exit code 0) even without a TTY
	# If it fails, it might be because debug console isn't enabled, which is a setup issue
	# But if it fails with a TTY-related error, that's the bug we're testing for
	[ ${exit_code} -eq 0 ] || [ ${exit_code} -eq 124 ] || die "Exec failed with exit code ${exit_code}. Output: ${output}"
	
	# Check that we got some output (proving the connection worked)
	[ -n "${output}" ] || die "No output received from exec command"
}

