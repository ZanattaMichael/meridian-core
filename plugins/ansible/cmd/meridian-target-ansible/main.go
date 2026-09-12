// Command meridian-target-ansible serves the Ansible emitter over Meridian's
// plugin protocol.
//
// There is deliberately nothing here but the wiring. Everything the target
// decides lives in the ansible package, which knows nothing about processes or
// gRPC, so the emitter can be tested without a boundary and the boundary can be
// tested without a target.
package main

import (
	"fmt"
	"os"

	"github.com/ZanattaMichael/meridian-core/pkg/sdk"
	"github.com/ZanattaMichael/meridian-core/plugins/ansible"
)

func main() {
	if err := sdk.Serve(ansible.New()); err != nil {
		// Stdout belongs to the protocol, so a failure goes to stderr, where
		// the host collects it and reports it alongside the load error.
		fmt.Fprintln(os.Stderr, "meridian-target-ansible:", err)
		os.Exit(1)
	}
}
