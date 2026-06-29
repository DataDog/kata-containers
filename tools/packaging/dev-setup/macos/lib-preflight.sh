#!/usr/bin/env bash
# Sourced by host-side macOS scripts; call preflight_checks() at startup.
# Each check fails fast with an actionable message.

preflight_checks() {
    _pf_macos
    _pf_apple_silicon
    _pf_macos_version
    _pf_nested_virtualization
    _pf_homebrew
    _pf_lima
    _pf_tools
    _pf_disk
}

_pf_macos() {
    if [[ "$(uname -s)" != Darwin ]]; then
        echo "error: this tool targets macOS; for Linux see docs/Developer-Guide.md" >&2
        exit 1
    fi
}

_pf_apple_silicon() {
    if [[ "$(uname -m)" != arm64 ]]; then
        echo "error: Apple Silicon (arm64) required — vmType: vz with nestedVirtualization is exclusive to Apple Silicon" >&2
        exit 1
    fi
}

_pf_macos_version() {
    local ver major
    ver=$(sw_vers -productVersion)
    major=$(echo "${ver}" | cut -d. -f1)
    if (( major < 15 )); then
        echo "error: macOS 15 (Sequoia) or later required (have ${ver}) — nested virtualization requires macOS 15+" >&2
        exit 1
    fi
}

_pf_homebrew() {
    if ! command -v brew &>/dev/null; then
        echo "error: Homebrew not found — install from https://brew.sh:" >&2
        echo "  /bin/bash -c \"\$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\"" >&2
        exit 1
    fi
}

_pf_lima() {
    if ! command -v limactl &>/dev/null; then
        echo "error: limactl not found — install with: brew install lima" >&2
        exit 1
    fi
    local ver major minor
    ver=$(limactl --version 2>/dev/null | awk '{print $NF}')
    if [[ -z "${ver}" ]]; then
        echo "error: could not determine Lima version — is limactl working?" >&2
        exit 1
    fi
    major=$(echo "${ver}" | cut -d. -f1)
    minor=$(echo "${ver}" | cut -d. -f2)
    if [[ "${major}" -eq 0 && "${minor}" -lt 20 ]]; then
        echo "error: Lima 0.20+ required for vmType: vz with nestedVirtualization (have ${ver}) — run: brew upgrade lima" >&2
        exit 1
    fi
}

_pf_tools() {
    local missing=()
    for t in curl git; do
        command -v "${t}" &>/dev/null || missing+=("${t}")
    done
    if (( ${#missing[@]} > 0 )); then
        echo "error: required tools not found: ${missing[*]}" >&2
        exit 1
    fi
}

_pf_disk() {
    local avail
    avail=$(/bin/df -g / | awk 'NR==2 {print $4}')
    if (( avail < 80 )); then
        echo "warning: only ${avail}GiB free on / — kata-dev.yaml requests an 80GiB VM disk; low disk may cause provision failures" >&2
    fi
}

_pf_nested_virtualization() {
    local chip
    chip=$(sysctl -n machdep.cpu.brand_string)
    if [[ "${chip}" =~ ^Apple\ M[12]($|\ ) ]]; then
        echo "error: nested virtualization requires Apple M3 or later (have ${chip})" >&2
        exit 1
    fi
}
