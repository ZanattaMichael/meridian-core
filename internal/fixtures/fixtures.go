// Package fixtures holds the documents every target's tests compile.
//
// It exists so that "read the two targets' output side by side" means something
// exact. When each emitter kept its own fixtures, two golden files could drift
// apart by a param nobody noticed changing, and the comparison would quietly
// stop being a comparison. Here there is one definition, and a golden file per
// target is a rendering of the same document.
//
// That matters most where the targets disagree. A document Ansible compiles and
// Puppet refuses is the clearest evidence a capability difference is real; it is
// only evidence at all if both were handed identical input.
//
// The documents are realistic rather than minimal. Golden files are the record
// of what a target actually produces, so they have to look like work an
// operator would recognise.
package fixtures

import (
	"sort"

	"github.com/ZanattaMichael/meridian-core/internal/ir"
)

// Host is the resolved target host every fixture compiles against.
const Host = "web01.example.com"

// Name is the document name every fixture carries, and therefore the name that
// appears in a play, a class, or whatever else a target derives from it.
const Name = "web"

// All is every shared fixture, keyed by the name its golden files use.
//
// A target is expected to compile each of them or to refuse with a reason. It is
// not expected to compile all of them: refusing a document a target genuinely
// cannot express is the correct outcome, and the refusal is worth a golden file
// of its own.
var All = map[string][]ir.Resource{
	// web_stack exercises the ordinary shape of a configuration document, and
	// one notify action — reload — that not every target can perform.
	"web_stack": {
		{ID: "nginx_pkg", Type: "package", Params: map[string]any{"name": "nginx"}},
		{ID: "nginx_conf", Type: "file",
			Params: map[string]any{
				"path":    "/etc/nginx/nginx.conf",
				"content": "worker_processes 4;\n",
				"owner":   "root",
				"mode":    "0644",
			},
			DependsOn: []ir.Dependency{{Resource: "nginx_pkg"}},
			Notifies:  []ir.Notification{{Resource: "nginx_svc", Action: "restart"}}},
		{ID: "nginx_site", Type: "file",
			Params:    map[string]any{"path": "/etc/nginx/sites-enabled/app", "content": "server {}\n"},
			DependsOn: []ir.Dependency{{Resource: "nginx_pkg"}},
			Notifies:  []ir.Notification{{Resource: "nginx_svc", Action: "reload"}}},
		{ID: "nginx_svc", Type: "service", State: "running",
			Params:    map[string]any{"name": "nginx", "enabled": true},
			DependsOn: []ir.Dependency{{Resource: "nginx_pkg"}}},
	},

	// accounts exercises users, groups and directories: no notification at all,
	// a deep dependency chain, and params of several types including a list.
	"accounts": {
		{ID: "deploy_group", Type: "group", Params: map[string]any{"name": "deploy", "gid": 1200}},
		{ID: "deploy_user", Type: "user",
			Params: map[string]any{
				"name":   "deploy",
				"uid":    1200,
				"shell":  "/bin/bash",
				"groups": []any{"deploy", "sudo"},
			},
			DependsOn: []ir.Dependency{{Resource: "deploy_group"}}},
		{ID: "legacy_user", Type: "user", State: "absent", Params: map[string]any{"name": "old"}},
		{ID: "home_dir", Type: "file", State: "directory",
			Params:    map[string]any{"path": "/srv/app", "owner": "deploy", "group": "deploy", "mode": "0750"},
			DependsOn: []ir.Dependency{{Resource: "deploy_user"}}},
	},

	// restart_chain exercises the notify actions a target is most likely to
	// support: restarting a service and running a command. Both are triggered by
	// one resource, so it also covers a resource notifying more than one thing.
	"restart_chain": {
		{ID: "app_pkg", Type: "package", Params: map[string]any{"name": "app"}},
		{ID: "app_conf", Type: "file",
			Params: map[string]any{
				"path":    "/etc/app/app.conf",
				"content": "mode = fast\nworkers = 8\n",
				"mode":    "0640",
			},
			DependsOn: []ir.Dependency{{Resource: "app_pkg"}},
			Notifies: []ir.Notification{
				{Resource: "app_svc", Action: "restart"},
				{Resource: "warm_cache", Action: "run"},
			}},
		{ID: "app_svc", Type: "service", State: "running",
			Params:    map[string]any{"name": "app", "enabled": true},
			DependsOn: []ir.Dependency{{Resource: "app_pkg"}}},
		{ID: "warm_cache", Type: "exec",
			Params:    map[string]any{"command": "/usr/local/bin/warm-cache --all"},
			DependsOn: []ir.Dependency{{Resource: "app_svc"}}},
	},

	// runtime_condition exercises the one thing Meridian leaves to the node:
	// a condition evaluated at the instant of apply rather than at compile time.
	// A target with no apply-time conditional cannot compile this at all.
	"runtime_condition": {
		{ID: "stale_conf", Type: "file", State: "absent", Params: map[string]any{"path": "/etc/old-app.conf"}},
		{ID: "warm_cache", Type: "exec",
			Params:      map[string]any{"command": "/usr/local/bin/warm-cache --all", "chdir": "/srv/app"},
			RuntimeWhen: "ansible_facts['os_family'] == 'Debian'",
			DependsOn:   []ir.Dependency{{Resource: "stale_conf"}}},
		{ID: "reindex", Type: "exec",
			Params:    map[string]any{"command": "a | b", "shell": true},
			DependsOn: []ir.Dependency{{Resource: "warm_cache"}}},
	},
}

// Names returns every fixture name, sorted, so a test that iterates them runs
// its subtests in the same order every time.
func Names() []string {
	out := make([]string, 0, len(All))
	for name := range All {
		out = append(out, name)
	}
	sort.Strings(out)
	return out
}
