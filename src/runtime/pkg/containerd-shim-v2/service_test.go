// Copyright (c) 2021 Intel Corporation
//
// SPDX-License-Identifier: Apache-2.0
//

package containerdshim

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	taskAPI "github.com/containerd/containerd/api/runtime/task/v2"
	ktu "github.com/kata-containers/kata-containers/src/runtime/pkg/katatestutils"
	"github.com/stretchr/testify/assert"
)

func newService(id string) (*service, error) {
	ctx := context.Background()

	ctx, cancel := context.WithCancel(ctx)

	s := &service{
		id:         id,
		pid:        uint32(os.Getpid()),
		ctx:        ctx,
		containers: make(map[string]*container),
		events:     make(chan interface{}, chSize),
		ec:         make(chan exit, bufferSize),
		cancel:     cancel,
	}

	return s, nil
}

func TestServiceCreate(t *testing.T) {
	const badCIDErrorPrefix = "invalid container/sandbox ID"
	const blankCIDError = "ID cannot be blank"

	assert := assert.New(t)

	_, bundleDir, _ := ktu.SetupOCIConfigFile(t)

	ctx := context.Background()

	s, err := newService("foo")
	assert.NoError(err)

	for i, d := range ktu.ContainerIDTestData {
		msg := fmt.Sprintf("test[%d]: %+v", i, d)

		// Only consider error scenarios as we are only testing invalid CIDs here.
		if d.Valid {
			continue
		}

		task := taskAPI.CreateTaskRequest{
			ID:     d.ID,
			Bundle: bundleDir,
		}

		_, err = s.Create(ctx, &task)
		assert.Error(err, msg)

		if d.ID == "" {
			assert.Equal(err.Error(), blankCIDError, msg)
		} else {
			assert.True(strings.HasPrefix(err.Error(), badCIDErrorPrefix), msg)
		}
	}
}

func TestCheckpointIDFromPath(t *testing.T) {
	t.Parallel()

	cases := []struct {
		input    string
		expected string
	}{
		{"/tmp/demo.tar.gz", "demo.tar"},
		{"subdir/my checkpoint", "my-checkpoint"},
		{"../foo_bar", "foo_bar"},
	}

	for _, tt := range cases {
		t.Run(tt.input, func(t *testing.T) {
			got := checkpointIDFromPath(tt.input)
			if got != tt.expected {
				t.Fatalf("expected %q, got %q", tt.expected, got)
			}
		})
	}

	// When no base name is provided we only validate the output
	got := checkpointIDFromPath("")
	if got == "" {
		t.Fatal("expected checkpointIDFromPath to generate non-empty id for empty input")
	}
	if strings.Contains(got, string(os.PathSeparator)) {
		t.Fatalf("generated id %q contains path separators", got)
	}
}

func TestExportCheckpointDir(t *testing.T) {
	t.Parallel()

	src := t.TempDir()
	dst := filepath.Join(t.TempDir(), "checkpoint-export")

	writeFile := func(path, data string) {
		t.Helper()
		if err := os.WriteFile(path, []byte(data), 0o600); err != nil {
			t.Fatalf("write %s: %v", path, err)
		}
	}

	writeFile(filepath.Join(src, "state.json"), `{"pid":42}`)
	if err := os.Mkdir(filepath.Join(src, "subdir"), 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	writeFile(filepath.Join(src, "subdir", "dump.log"), "hello")

	if err := exportCheckpointDir(src, dst); err != nil {
		t.Fatalf("export checkpoint: %v", err)
	}

	check := func(path, want string) {
		t.Helper()
		b, err := os.ReadFile(path)
		if err != nil {
			t.Fatalf("read %s: %v", path, err)
		}
		if string(b) != want {
			t.Fatalf("expected %q in %s, got %q", want, path, string(b))
		}
	}

	check(filepath.Join(dst, "state.json"), `{"pid":42}`)
	check(filepath.Join(dst, "subdir", "dump.log"), "hello")

	// Update the source and ensure the export overwrites the destination.
	writeFile(filepath.Join(src, "state.json"), `{"pid":7}`)
	if err := exportCheckpointDir(src, dst); err != nil {
		t.Fatalf("export checkpoint (overwrite): %v", err)
	}
	check(filepath.Join(dst, "state.json"), `{"pid":7}`)
}
