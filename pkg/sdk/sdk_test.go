package sdk

import (
	"encoding/json"
	"strings"
	"testing"
)

func TestArtifactPathsAreSorted(t *testing.T) {
	a := Artifact{Files: map[string]string{"z.yml": "", "a.ini": "", "m.txt": ""}}
	got := a.Paths()
	want := []string{"a.ini", "m.txt", "z.yml"}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("paths = %v, want %v", got, want)
		}
	}
}

func TestWarningNamesItsTargetAndResource(t *testing.T) {
	with := Warning{Target: "ansible", Resource: "nginx_svc", Msg: "flattened"}
	if with.String() != "ansible: nginx_svc: flattened" {
		t.Fatalf("warning = %q", with.String())
	}
	without := Warning{Target: "ansible", Msg: "flattened"}
	if without.String() != "ansible: flattened" {
		t.Fatalf("warning = %q", without.String())
	}
}

func TestPositionRendersUnknownInputReadably(t *testing.T) {
	if got := (Position{}).String(); got != "<input>" {
		t.Fatalf("position = %q", got)
	}
	if got := (Position{File: "site.yaml", DocIdx: 2, Line: 4, Column: 1}).String(); got != "site.yaml[doc 2]:4:1" {
		t.Fatalf("position = %q", got)
	}
	if (Position{}).Known() {
		t.Fatal("an empty position should not claim to be known")
	}
}

func TestEdgeKindNamesTheFieldItCameFrom(t *testing.T) {
	if HardOrder.String() != "dependsOn" || Notify.String() != "notifies" {
		t.Fatal("edge kinds should render as the IR field an author wrote")
	}
	if !strings.Contains(EdgeKind(9).String(), "9") {
		t.Fatal("an unknown edge kind should still render its value")
	}
}

func TestGraphLookupsUseTheTreeItWasGiven(t *testing.T) {
	g := sampleGraph()
	if got := g.Order(); len(got) != 2 || got[0] != "nginx_pkg" {
		t.Fatalf("order = %v", got)
	}
	if _, ok := g.Resource("nginx_svc"); !ok {
		t.Fatal("a declared resource should be findable")
	}
	if _, ok := g.Resource("absent"); ok {
		t.Fatal("an undeclared resource should not be findable")
	}
	if got := g.EdgesOfKind(Notify); len(got) != 1 || got[0].Action != "restart" {
		t.Fatalf("notify edges = %+v", got)
	}
}

// TestCanonicalValueIsStableAcrossMapOrder is the determinism oracle itself: it
// has to be insensitive to Go's map iteration, or every test that relies on it
// is only passing by luck.
func TestCanonicalValueIsStableAcrossMapOrder(t *testing.T) {
	v := map[string]any{"b": 2, "a": 1, "c": map[string]any{"z": true, "y": []any{3, "x"}}}
	want := CanonicalValue(v)
	for i := 0; i < 200; i++ {
		if got := CanonicalValue(v); got != want {
			t.Fatalf("run %d: %s, want %s", i, got, want)
		}
	}
	if !strings.HasPrefix(want, `{"a":1,"b":2`) {
		t.Fatalf("canonical form is not sorted: %s", want)
	}
}

func TestDecodeValuesRejectsSomethingThatIsNotAnObject(t *testing.T) {
	if _, err := decodeValues([]byte(`["a"]`)); err == nil {
		t.Fatal("a JSON array is not a params object and should be refused")
	}
	if _, err := decodeValues([]byte(`{`)); err == nil {
		t.Fatal("malformed JSON should be refused")
	}
}

func TestDecodeValuesTreatsAbsentAsEmpty(t *testing.T) {
	got, err := decodeValues(nil)
	if err != nil || got == nil || len(got) != 0 {
		t.Fatalf("decodeValues(nil) = %v, %v; want an empty map", got, err)
	}
}

// TestAnUnrepresentableNumberStaysExact: a literal too large for int64 or
// float64 keeps its digits rather than silently losing precision.
func TestAnUnrepresentableNumberStaysExact(t *testing.T) {
	const huge = "123456789012345678901234567890123456789012345678901234567890"
	got, err := decodeValues([]byte(`{"n":` + huge + `e4000}`))
	if err != nil {
		t.Fatalf("decodeValues: %v", err)
	}
	if s, ok := got["n"].(string); !ok || !strings.HasPrefix(s, "123456789") {
		t.Fatalf("n = %#v, want the literal preserved", got["n"])
	}
}

func TestEncodeValuesTreatsNilAsAnEmptyObject(t *testing.T) {
	b, err := encodeValues(nil)
	if err != nil {
		t.Fatalf("encodeValues: %v", err)
	}
	if string(b) != "{}" {
		t.Fatalf("encoded = %s, want {}", b)
	}
	var out map[string]any
	if err := json.Unmarshal(b, &out); err != nil {
		t.Fatalf("the encoding is not valid JSON: %v", err)
	}
}

func TestErrorfCarriesTheResourcePosition(t *testing.T) {
	err := Errorf(Resource{ID: "r", Pos: Position{File: "site.yaml", Line: 3}}, "some-rule", "nope %d", 1)
	if err.Resource != "r" || err.Rule != "some-rule" || err.Msg != "nope 1" {
		t.Fatalf("error = %+v", err)
	}
	if !strings.HasPrefix(err.Error(), "site.yaml:3: ") {
		t.Fatalf("error should lead with the position: %q", err.Error())
	}
}
