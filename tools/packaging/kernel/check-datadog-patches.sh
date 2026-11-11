#!/usr/bin/env bash
#
# Copyright (c) 2026 Datadog, Inc.
#
# SPDX-License-Identifier: Apache-2.0
#
# Fail if the patch directory for a kernel version does not contain every
# Datadog patch listed in patches/datadog-required-patches.txt.
#
# A kernel bump silently drops a patch when the new patches/<major>.<minor>.x/
# directory does not carry it (see #140). This check makes that a CI failure.
#
# Usage:
#   KERNEL_VERSION=v6.12.115 ./check-datadog-patches.sh
#   ./check-datadog-patches.sh v6.12.115
#
# Environment:
#   KERNEL_VERSION  Kernel version or tag (for example v6.12.115). The first
#                   argument overrides it.
#   PATCHES_DIR     Patches root (default: <script dir>/patches).
#   MANIFEST        Required-patch list
#                   (default: ${PATCHES_DIR}/datadog-required-patches.txt).

set -o errexit
set -o nounset
set -o pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly script_dir

kernel_version="${1:-${KERNEL_VERSION:-}}"
patches_dir="${PATCHES_DIR:-${script_dir}/patches}"
manifest="${MANIFEST:-${patches_dir}/datadog-required-patches.txt}"

die() {
	echo "ERROR: $*" >&2
	exit 1
}

[[ -n "${kernel_version}" ]] || die "set KERNEL_VERSION or pass the kernel version as the first argument"
[[ -f "${manifest}" ]] || die "manifest not found: ${manifest}"

# Resolve the directory the same way build-kernel.sh does: strip a leading
# 'v', then keep <major>.<minor> (see get_major_kernel_version and setup_kernel).
kernel_version="${kernel_version#v}"
major="$(echo "${kernel_version}" | cut -d. -f1)"
minor="$(echo "${kernel_version}" | cut -d. -f2)"
[[ "${major}" =~ ^[0-9]+$ && "${minor}" =~ ^[0-9]+$ ]] ||
	die "cannot parse <major>.<minor> from kernel version '${kernel_version}'"
version_dir="${patches_dir}/${major}.${minor}.x"

echo "INFO: kernel version ${kernel_version} uses ${version_dir}"
[[ -d "${version_dir}" ]] || die "patch directory does not exist: ${version_dir}"

required=()
while IFS= read -r line || [[ -n "${line}" ]]; do
	line="${line%%#*}"
	line="$(echo "${line}" | tr -d '[:space:]')"
	if [[ -n "${line}" ]]; then
		required+=("${line}")
	fi
done <"${manifest}"

[[ ${#required[@]} -gt 0 ]] || die "manifest lists no patches: ${manifest}"

missing=()
for patch in "${required[@]}"; do
	if [[ -f "${version_dir}/${patch}" ]]; then
		echo "OK:   ${patch}"
	else
		echo "MISS: ${patch}"
		missing+=("${patch}")
	fi
done

if [[ ${#missing[@]} -gt 0 ]]; then
	{
		echo
		echo "ERROR: ${#missing[@]} required Datadog kernel patch(es) missing from ${version_dir}:"
		printf '  - %s\n' "${missing[@]}"
		echo
		echo "Port each patch to ${major}.${minor}.x before you bump KERNEL_VERSION."
		echo "If upstream now contains the fix, confirm that it activates in our setup,"
		echo "then remove the entry from ${manifest} in the same PR."
		echo "Guide: .claude/skills/porting-kernel-patches/SKILL.md"
	} >&2
	exit 1
fi

echo "INFO: all ${#required[@]} required Datadog kernel patch(es) are present"
