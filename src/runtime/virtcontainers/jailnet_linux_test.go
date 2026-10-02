//go:build linux

// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0

package virtcontainers

import (
	"context"
	"fmt"
	"path/filepath"
	"testing"

	"github.com/containernetworking/plugins/pkg/ns"
	"github.com/containernetworking/plugins/pkg/testutils"
	ktu "github.com/kata-containers/kata-containers/src/runtime/pkg/katatestutils"
	"github.com/stretchr/testify/require"
	"github.com/vishvananda/netlink"
)

func TestSetupJailNetNetworkingPreservesMTU(t *testing.T) {
	if tc.NotValid(ktu.NeedRoot()) {
		t.Skip(testDisabledAsNonRoot)
	}

	for _, mtu := range []int{1280, 1500, 9001} {
		t.Run(fmt.Sprintf("MTU%d", mtu), func(t *testing.T) {
			podNS, err := testutils.NewNS()
			require.NoError(t, err)
			defer podNS.Close()
			defer testutils.UnmountNS(podNS)

			err = podNS.Do(func(_ ns.NetNS) (retErr error) {
				// A dummy endpoint is enough to exercise the real TAP/veth setup
				// without involving the host network or starting a VM.
				link := &netlink.Dummy{LinkAttrs: netlink.LinkAttrs{Name: "eth0", MTU: mtu}}
				if err := netlink.LinkAdd(link); err != nil {
					return err
				}
				endpoint, err := createVethNetworkEndpoint(0, "eth0", NetXConnectJailNetModel)
				if err != nil {
					return err
				}
				endpoint.SetProperties(NetworkInfo{Iface: NetlinkIface{LinkAttrs: netlink.LinkAttrs{MTU: mtu}}})
				ctx := context.Background()
				defer func() {
					for _, fd := range endpoint.NetPair.VMFds {
						_ = fd.Close()
					}
					if err := removeJailNetNetworking(ctx, endpoint); retErr == nil {
						retErr = err
					}
				}()
				if err := setupJailNetNetworking(ctx, endpoint, 1, true); err != nil {
					return err
				}

				checkMTU := func(name string) error {
					link, err := netlink.LinkByName(name)
					if err != nil {
						return err
					}
					if got := link.Attrs().MTU; got != mtu {
						return fmt.Errorf("%s MTU = %d, want %d", name, got, mtu)
					}
					return nil
				}
				if err := checkMTU(proxyPodVethName); err != nil {
					return err
				}
				jailNS, err := ns.GetNS(filepath.Join("/run/netns", jailNetnsName(endpoint.NetPair.ID)))
				if err != nil {
					return err
				}
				defer jailNS.Close()
				if err := jailNS.Do(func(_ ns.NetNS) error {
					if err := checkMTU(proxyJailVethName); err != nil {
						return err
					}
					return checkMTU(endpoint.NetPair.TAPIface.Name)
				}); err != nil {
					return err
				}
				interfaces, _, _, err := generateVCNetworkStructures(ctx, []Endpoint{endpoint})
				if err != nil {
					return err
				}
				if len(interfaces) != 1 || interfaces[0].Mtu != uint64(mtu) {
					return fmt.Errorf("guest interfaces do not preserve MTU %d: %v", mtu, interfaces)
				}
				return nil
			})
			require.NoError(t, err)
		})
	}
}
