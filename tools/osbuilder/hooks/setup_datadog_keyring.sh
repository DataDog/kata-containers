#!/bin/sh
set -e

dd_keyring="/tmp/datadog-archive-keyring.gpg"
curl -fsSL https://keys.datadoghq.com/DATADOG_APT_KEY_CURRENT.public | gpg --dearmor > "${dd_keyring}"
