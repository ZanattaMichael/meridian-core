package ansible

import (
	"flag"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"github.com/ZanattaMichael/meridian-core/internal/ast"
	"github.com/ZanattaMichael/meridian-core/internal/ir"
	"gopkg.in/yaml.v3"
)

var update = flag.Bool("update", false, "rewrite the golden files from current output")

// fixtures are realistic documents, not minimal ones: the golden files are the
// record of what this target actually produces, so they have to look like work.
var fixtures = map[string][]ir.Resource{
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

func TestGoldenFiles(t *testing.T) {
	for name, resources := range fixtures {
		t.Run(name, func(t *testing.T) {
			files := emit(t, resources...)
			for _, path := range []string{PlaybookPath, InventoryPath} {
				golden := filepath.Join("testdata", name+"."+strings.ReplaceAll(path, ".", "_"))
				got := files[path]
				if *update {
					if err := os.WriteFile(golden, []byte(got), 0o644); err != nil {
						t.Fatalf("writing %s: %v", golden, err)
					}
					continue
				}
				want, err := os.ReadFile(golden)
				if err != nil {
					t.Fatalf("reading %s (run go test -update to create it): %v", golden, err)
				}
				if got != string(want) {
					t.Errorf("%s does not match %s.\n--- got ---\n%s\n--- want ---\n%s",
						path, golden, got, want)
				}
			}
		})
	}
}

// TestNoConditionalConstructsLeak is the automated check the testing plan asks
// for: a compile-time `when` must be evaluated and pruned upstream, so the only
// conditional allowed in emitted output is one an author explicitly asked for
// with runtimeWhen.
func TestNoConditionalConstructsLeak(t *testing.T) {
	for name, resources := range fixtures {
		t.Run(name, func(t *testing.T) {
			runtimeConditions := 0
			for _, r := range resources {
				if r.RuntimeWhen != "" {
					runtimeConditions++
				}
			}
			playbook := emit(t, resources...)[PlaybookPath]
			if got := strings.Count(playbook, "when:"); got != runtimeConditions {
				t.Fatalf("playbook holds %d conditionals, want %d (one per runtimeWhen):\n%s",
					got, runtimeConditions, playbook)
			}
		})
	}
}

func TestEmittedPlaybookParsesAsYAML(t *testing.T) {
	for name, resources := range fixtures {
		t.Run(name, func(t *testing.T) {
			var plays []map[string]any
			if err := yaml.Unmarshal([]byte(emit(t, resources...)[PlaybookPath]), &plays); err != nil {
				t.Fatalf("emitted playbook is not valid YAML: %v", err)
			}
			if len(plays) != 1 {
				t.Fatalf("expected exactly one play, got %d", len(plays))
			}
			if plays[0]["hosts"] != "web01.example.com" {
				t.Fatalf("play hosts = %v", plays[0]["hosts"])
			}
			tasks, ok := plays[0]["tasks"].([]any)
			if !ok || len(tasks) != len(resources) {
				t.Fatalf("expected %d tasks, got %v", len(resources), plays[0]["tasks"])
			}
		})
	}
}

func TestInventoryNamesTheResolvedHost(t *testing.T) {
	got := emit(t, ir.Resource{ID: "pkg", Type: "package", Params: map[string]any{"name": "nginx"}})[InventoryPath]
	if got != "[meridian]\nweb01.example.com\n" {
		t.Fatalf("inventory = %q", got)
	}
}

func TestArtifactPathsAreSorted(t *testing.T) {
	art, _, err := New().Emit(buildAST(t,
		ir.Resource{ID: "pkg", Type: "package", Params: map[string]any{"name": "nginx"}}), nil)
	if err != nil {
		t.Fatalf("Emit: %v", err)
	}
	got := art.Paths()
	if len(got) != 2 || got[0] != InventoryPath || got[1] != PlaybookPath {
		t.Fatalf("paths = %v", got)
	}
}

// TestEmitIsPureRegardlessOfHowTheTreeWasBuilt asserts emit holds no state of
// its own: a tree assembled by hand renders exactly like the same tree derived
// from a document.
func TestEmitIsPureRegardlessOfHowTheTreeWasBuilt(t *testing.T) {
	fromDocument := buildAST(t, fixtures["web_stack"]...)

	byHand := ast.New(fromDocument.Name, fromDocument.Host, fromDocument.Target,
		fromDocument.Resources(), fromDocument.Edges())

	a, _, err := New().Emit(fromDocument, nil)
	if err != nil {
		t.Fatalf("Emit: %v", err)
	}
	b, _, err := New().Emit(byHand, nil)
	if err != nil {
		t.Fatalf("Emit: %v", err)
	}
	for _, path := range a.Paths() {
		if a.Files[path] != b.Files[path] {
			t.Fatalf("%s differs between an equal pair of trees:\n%s\n---\n%s",
				path, a.Files[path], b.Files[path])
		}
	}
}

func TestEmitIsByteIdenticalAcrossRuns(t *testing.T) {
	want := emit(t, fixtures["web_stack"]...)[PlaybookPath]
	for i := 0; i < 100; i++ {
		if got := emit(t, fixtures["web_stack"]...)[PlaybookPath]; got != want {
			t.Fatalf("run %d differed:\n%s", i, got)
		}
	}
}

func TestEmitIsByteIdenticalUnderConcurrency(t *testing.T) {
	want := emit(t, fixtures["web_stack"]...)[PlaybookPath]
	got := make([]string, 16)
	var wg sync.WaitGroup
	for i := range got {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			g, err := tryBuildAST(fixtures["web_stack"]...)
			if err != nil {
				got[i] = "error: " + err.Error()
				return
			}
			art, _, err := New().Emit(g, nil)
			if err != nil {
				got[i] = "error: " + err.Error()
				return
			}
			got[i] = art.Files[PlaybookPath]
		}(i)
	}
	wg.Wait()
	for i, g := range got {
		if g != want {
			t.Fatalf("goroutine %d differed:\n%s", i, g)
		}
	}
}
