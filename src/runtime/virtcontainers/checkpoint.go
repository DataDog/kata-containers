// Copyright (c) 2025
//
// SPDX-License-Identifier: Apache-2.0

package virtcontainers

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

const checkpointDirName = "checkpoints"

// CheckpointConfig configures checkpoint/restore behaviour for a sandbox.
type CheckpointConfig struct {
	Enable bool

	// GuestDir is the directory inside the guest where checkpoint images are staged.
	GuestDir string

	// GuestCriuPath points to the CRIU binary inside the guest.
	GuestCriuPath string

	// HostDir is the host directory (relative to the sandbox shared path) used
	// to stage checkpoint artifacts.
	HostDir string
}

// CheckpointRequest captures the parameters required to checkpoint a container.
type CheckpointRequest struct {
	ContainerID        string
	CheckpointID       string
	ParentCheckpointID string
	LeaveRunning       bool
	RuntimeOptions     []byte
}

// CheckpointResult describes the on-host and in-guest paths for a new checkpoint artifact.
type CheckpointResult struct {
	HostDir  string
	GuestDir string
}

// RestoreRequest describes how to restore a container from an existing checkpoint bundle.
type RestoreRequest struct {
	ContainerID        string
	CheckpointID       string
	ParentCheckpointID string
	RuntimeOptions     []byte

	// SourceHostDir points to the host directory, under the sandbox shared path,
	// containing the CRIU image set to be restored.
	SourceHostDir string
}

// validate ensures the config is internally consistent.
func (c CheckpointConfig) validate() error {
	if !c.Enable {
		return nil
	}

	if c.GuestDir == "" {
		return fmt.Errorf("checkpoint guest directory must be specified when enabled")
	}

	if c.GuestCriuPath == "" {
		return fmt.Errorf("checkpoint CRIU path must be specified when enabled")
	}

	if c.HostDir == "" {
		return fmt.Errorf("checkpoint host directory must be specified when enabled")
	}

	return nil
}

// checkpointHostBase returns the host directory used as the root for checkpoint staging.
func (s *Sandbox) checkpointHostBase() string {
	return filepath.Join(GetSharePath(s.id), checkpointDirName)
}

func (s *Sandbox) checkpointGuestBase() string {
	if s.config.Checkpoint.GuestDir != "" {
		return s.config.Checkpoint.GuestDir
	}
	return filepath.Join(kataGuestSharedDir(), checkpointDirName)
}

// resolveCheckpointDirs returns the host and guest directories used to stage
// checkpoint data for a given container/checkpoint identifier.
func (s *Sandbox) resolveCheckpointDirs(containerID, checkpointID string) (host string, guest string) {
	host = filepath.Join(s.checkpointHostBase(), containerID, checkpointID)
	guest = filepath.Join(s.checkpointGuestBase(), containerID, checkpointID)
	return host, guest
}

func (s *Sandbox) guestPathForHost(hostPath string) (string, error) {
	shareRoot := GetSharePath(s.id)
	rel, err := filepath.Rel(shareRoot, hostPath)
	if err != nil {
		return "", fmt.Errorf("failed to compute guest path for %q: %w", hostPath, err)
	}

	if rel == ".." || strings.HasPrefix(rel, fmt.Sprintf("..%c", os.PathSeparator)) {
		return "", fmt.Errorf("path %q is outside sandbox shared directory %q", hostPath, shareRoot)
	}

	return filepath.Join(kataGuestSharedDir(), rel), nil
}
