package ir

// APIVersionV1 is the only apiVersion this build understands. Any other value
// is an explicit error rather than a silent partial parse.
const APIVersionV1 = "meridian/v1"

// Kind discriminates the two tiers of the IR: provisioning and host config.
type Kind string

const (
	KindInfrastructure Kind = "Infrastructure"
	KindResourceSet    Kind = "ResourceSet"
)

// IfMissing is the policy applied when an edge points at a resource that is not
// present in the compiled graph (typically because `when` pruned it).
type IfMissing string

const (
	// IfMissingError is the default: a dangling edge is an authoring mistake.
	IfMissingError IfMissing = "error"
	IfMissingSkip  IfMissing = "skip"
)

// Metadata carries document identity.
type Metadata struct {
	Name   string            `yaml:"name" json:"name"`
	Labels map[string]string `yaml:"labels,omitempty" json:"labels,omitempty"`
}

// Dependency is a hard ordering constraint: the referenced resource must be
// applied before the resource declaring it.
type Dependency struct {
	Resource  string    `yaml:"resource" json:"resource"`
	IfMissing IfMissing `yaml:"ifMissing,omitempty" json:"ifMissing,omitempty"`

	Pos Position `yaml:"-" json:"-"`
}

// Notification is a conditional, deferred trigger. It is deliberately a
// separate type from Dependency because it is not an ordering constraint.
type Notification struct {
	Resource  string    `yaml:"resource" json:"resource"`
	On        string    `yaml:"on,omitempty" json:"on,omitempty"`
	Action    string    `yaml:"action" json:"action"`
	IfMissing IfMissing `yaml:"ifMissing,omitempty" json:"ifMissing,omitempty"`

	Pos Position `yaml:"-" json:"-"`
}

// Resource is one declared unit of desired state.
type Resource struct {
	ID          string         `yaml:"id" json:"id"`
	Type        string         `yaml:"type" json:"type"`
	State       string         `yaml:"state,omitempty" json:"state,omitempty"`
	Params      map[string]any `yaml:"params,omitempty" json:"params,omitempty"`
	DependsOn   []Dependency   `yaml:"dependsOn,omitempty" json:"dependsOn,omitempty"`
	Notifies    []Notification `yaml:"notifies,omitempty" json:"notifies,omitempty"`
	When        string         `yaml:"when,omitempty" json:"when,omitempty"`
	RuntimeWhen string         `yaml:"runtimeWhen,omitempty" json:"runtimeWhen,omitempty"`

	// Index is the resource's position in its document's declaration order. It
	// is the sole tiebreak for topological sort, so downstream ordering never
	// depends on map iteration.
	Index int      `yaml:"-" json:"-"`
	Pos   Position `yaml:"-" json:"-"`
}

// Output is a value an Infrastructure document exposes to ResourceSet docs.
type Output struct {
	Name  string `yaml:"name" json:"name"`
	Value string `yaml:"value" json:"value"`

	Pos Position `yaml:"-" json:"-"`
}

// TargetHost binds a ResourceSet to the node it configures. `from:
// infrastructure` resolves Ref/Output against a sibling Infrastructure doc.
type TargetHost struct {
	From   string `yaml:"from" json:"from"`
	Ref    string `yaml:"ref,omitempty" json:"ref,omitempty"`
	Output string `yaml:"output,omitempty" json:"output,omitempty"`
	Host   string `yaml:"host,omitempty" json:"host,omitempty"`

	Pos Position `yaml:"-" json:"-"`
}

const (
	// TargetHostFromInfrastructure resolves against another document's outputs.
	TargetHostFromInfrastructure = "infrastructure"
	// TargetHostFromInline names an already-existing node directly.
	TargetHostFromInline = "inline"
)

// InfrastructureSpec is the body of a `kind: Infrastructure` document.
type InfrastructureSpec struct {
	Target        string     `yaml:"target" json:"target"`
	TargetVersion string     `yaml:"targetVersion,omitempty" json:"targetVersion,omitempty"`
	Resources     []Resource `yaml:"resources" json:"resources"`
	Outputs       []Output   `yaml:"outputs,omitempty" json:"outputs,omitempty"`
}

// Infrastructure is the provisioning tier: VMs, networks, and anything else
// that compiles to Terraform or ARM.
type Infrastructure struct {
	APIVersion string             `yaml:"apiVersion" json:"apiVersion"`
	Kind       Kind               `yaml:"kind" json:"kind"`
	Metadata   Metadata           `yaml:"metadata" json:"metadata"`
	Spec       InfrastructureSpec `yaml:"spec" json:"spec"`

	Pos Position `yaml:"-" json:"-"`
}

// ResourceSetSpec is the body of a `kind: ResourceSet` document.
type ResourceSetSpec struct {
	Target        string      `yaml:"target" json:"target"`
	TargetVersion string      `yaml:"targetVersion,omitempty" json:"targetVersion,omitempty"`
	TargetHost    *TargetHost `yaml:"targetHost,omitempty" json:"targetHost,omitempty"`
	Resources     []Resource  `yaml:"resources" json:"resources"`
}

// ResourceSet is the host-configuration tier: packages, files, services.
type ResourceSet struct {
	APIVersion string          `yaml:"apiVersion" json:"apiVersion"`
	Kind       Kind            `yaml:"kind" json:"kind"`
	Metadata   Metadata        `yaml:"metadata" json:"metadata"`
	Spec       ResourceSetSpec `yaml:"spec" json:"spec"`

	Pos Position `yaml:"-" json:"-"`
}

// Document is the common surface of both document kinds. It exists so the
// bundle-level checks can treat a mixed set of documents uniformly.
type Document interface {
	APIVersionOf() string
	KindOf() Kind
	NameOf() string
	TargetOf() string
	ResourcesOf() []Resource
	PositionOf() Position
}

func (d *Infrastructure) APIVersionOf() string    { return d.APIVersion }
func (d *Infrastructure) KindOf() Kind            { return d.Kind }
func (d *Infrastructure) NameOf() string          { return d.Metadata.Name }
func (d *Infrastructure) TargetOf() string        { return d.Spec.Target }
func (d *Infrastructure) ResourcesOf() []Resource { return d.Spec.Resources }
func (d *Infrastructure) PositionOf() Position    { return d.Pos }

// Output returns the named output and whether it was declared.
func (d *Infrastructure) Output(name string) (Output, bool) {
	for _, o := range d.Spec.Outputs {
		if o.Name == name {
			return o, true
		}
	}
	return Output{}, false
}

func (d *ResourceSet) APIVersionOf() string    { return d.APIVersion }
func (d *ResourceSet) KindOf() Kind            { return d.Kind }
func (d *ResourceSet) NameOf() string          { return d.Metadata.Name }
func (d *ResourceSet) TargetOf() string        { return d.Spec.Target }
func (d *ResourceSet) ResourcesOf() []Resource { return d.Spec.Resources }
func (d *ResourceSet) PositionOf() Position    { return d.Pos }

var (
	_ Document = (*Infrastructure)(nil)
	_ Document = (*ResourceSet)(nil)
)
