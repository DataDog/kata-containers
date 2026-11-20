// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0

package containerdshim

import "context"

type sandboxTeardownPhase uint8

const (
	teardownPending sandboxTeardownPhase = iota
	teardownStopDone
	teardownDeleteDone
)

// advanceTeardown serializes normal exit with the VMM watcher. Phases record
// completed attempts, including errors, matching the at-most-once contract.
// Callers may pause after Stop to publish exit before Delete; a watcher that
// already advanced further makes the request a no-op. Never acquire s.mu here.
func (s *service) advanceTeardown(ctx context.Context, target sandboxTeardownPhase) {
	s.teardownMu.Lock()
	defer s.teardownMu.Unlock()
	for s.teardownPhase < target {
		var err error
		switch s.teardownPhase {
		case teardownPending:
			err = s.sandbox.Stop(ctx, true)
		case teardownStopDone:
			err = s.sandbox.Delete(ctx)
		}
		if err != nil {
			shimLog.WithError(err).WithField("sandbox", s.sandbox.ID()).WithField("phase", s.teardownPhase).Error("sandbox teardown failed")
		}
		s.teardownPhase++
	}
}
