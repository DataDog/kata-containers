# Copyright (c) 2026 Datadog, Inc.
# SPDX-License-Identifier: Apache-2.0

input == "exceptions" {
	if (NF == 1 && $1 ~ /^CONFIG_[[:alnum:]_]+$/) allowed[$1] = 1
	next
}

{
	if (/^CONFIG_[[:alnum:]_]+=/) {
		symbol = setting = $0
		sub(/=.*/, "", symbol)
	} else if (/^# CONFIG_[[:alnum:]_]+ is not set$/) {
		symbol = $2
		setting = symbol "=n"
	} else {
		next
	}

	if (input == "fragments") requested[symbol] = setting
	else actual[symbol] = setting
}

END {
	for (symbol in requested) {
		if (!(symbol in allowed) && requested[symbol] != actual[symbol]) {
			print "Requested " requested[symbol] ", got " (actual[symbol] ? actual[symbol] : "no setting")
			failed = 1
		}
	}
	if (failed) exit 1
}
