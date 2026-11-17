//! The shape of a Meridian document, written down once as data.
//!
//! The parser reads the fields a mapping may hold from the tables here, so
//! what this module says a document may contain and what the compiler accepts
//! cannot drift apart. Everything that needs to know the language without
//! compiling a document reads the same tables: the JSON Schema export, the
//! formatter's field order, and the language server's completion and hover.
//!
//! Adding a field to the language means adding it to a table here and
//! decoding it in the parser. The tooling then offers it, documents it and
//! orders it with no further change.

use crate::types::{
    Kind, API_VERSION_V1, IMPORT_FROM_ARTIFACT, IMPORT_FROM_OUTPUT, NOTIFY_ON_CHANGED,
    TARGET_HOST_FROM_INFRASTRUCTURE, TARGET_HOST_FROM_INLINE,
};

/// One mapping in a document: the fields it may hold, in the order the
/// formatter writes them.
#[derive(Debug, PartialEq, Eq)]
pub struct Shape {
    /// A name for the shape, used as a definition name in the JSON Schema.
    pub name: &'static str,
    /// What the mapping is, in one or two sentences.
    pub doc: &'static str,
    pub fields: &'static [Field],
}

impl Shape {
    /// The field called `name`, if the shape has one.
    pub fn field(&self, name: &str) -> Option<&'static Field> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// Every field name, in canonical order.
    pub fn names(&self) -> Vec<&'static str> {
        self.fields.iter().map(|f| f.name).collect()
    }
}

/// One field of a [`Shape`].
#[derive(Debug, PartialEq, Eq)]
pub struct Field {
    pub name: &'static str,
    /// What the field means, written for the person filling it in.
    pub doc: &'static str,
    /// Whether validation refuses a document that leaves it out.
    pub required: bool,
    pub ty: Type,
    /// What the value refers to, when it is more than free text.
    pub role: Role,
}

/// What a field's value looks like.
#[derive(Debug, PartialEq, Eq)]
pub enum Type {
    /// A single string.
    String,
    /// A string that must be one of a fixed set.
    Enum(&'static [Choice]),
    /// A nested mapping.
    Object(&'static Shape),
    /// A list of mappings.
    List(&'static Shape),
    /// Yes or no.
    Bool,
    /// A list of strings, such as a command and its arguments.
    StringList,
    /// A mapping of names to strings, such as labels.
    StringMap,
    /// A free-form mapping. Its keys belong to the target, not the language.
    Params,
}

/// One value an [`Type::Enum`] field accepts.
#[derive(Debug, PartialEq, Eq)]
pub struct Choice {
    pub value: &'static str,
    pub doc: &'static str,
}

/// What a value refers to. A role is how tooling knows which names to offer
/// for a field, and where a reference leads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Free text or a fixed choice; nothing to resolve.
    None,
    /// The name of a target plugin.
    Target,
    /// A version of the target's tool.
    TargetVersion,
    /// The id a resource declares, which other resources refer to.
    ResourceId,
    /// A resource type of the document's target.
    ResourceType,
    /// A state of the resource's type.
    ResourceState,
    /// The params of the resource's type.
    ResourceParams,
    /// The id of another resource in the same document.
    ResourceRef,
    /// An action the notified resource can perform.
    NotifyAction,
    /// A condition Meridian decides at compile time.
    Condition,
    /// A condition the tool checks on the node at apply time.
    RuntimeCondition,
    /// The name an Infrastructure document declares.
    DocumentName,
    /// The name of an Infrastructure document in the same workflow.
    InfrastructureRef,
    /// The name an output is declared under.
    OutputName,
    /// The name of an output of the referenced Infrastructure document.
    OutputRef,
    /// Text that may hold `${resource.path}` references.
    Expression,
}

const fn text(name: &'static str, required: bool, role: Role, doc: &'static str) -> Field {
    Field {
        name,
        doc,
        required,
        ty: Type::String,
        role,
    }
}

const fn choice(value: &'static str, doc: &'static str) -> Choice {
    Choice { value, doc }
}

/// The values `apiVersion` accepts.
pub const API_VERSIONS: &[Choice] = &[choice(
    API_VERSION_V1,
    "The first version of the Meridian document language.",
)];

/// The values `kind` accepts.
pub const KINDS: &[Choice] = &[
    choice(
        "Infrastructure",
        "Provisions things that later documents are applied to: machines, networks, \
         clusters. Compiles for an infrastructure-as-code target and may declare outputs.",
    ),
    choice(
        "ResourceSet",
        "Describes the desired state of something that already exists: the packages, \
         files and services on a host, or the workloads on a cluster.",
    ),
];

/// The policies `ifMissing` accepts.
pub const IF_MISSING: &[Choice] = &[
    choice(
        "error",
        "Refuse to compile when the named resource is not in the compiled document. \
         This is the default.",
    ),
    choice(
        "skip",
        "Drop this edge when a `when` condition has removed the named resource.",
    ),
];

/// The values `targetHost.from` accepts.
pub const TARGET_HOST_FROM: &[Choice] = &[
    choice(
        TARGET_HOST_FROM_INLINE,
        "The host is named here, in `host`.",
    ),
    choice(
        TARGET_HOST_FROM_INFRASTRUCTURE,
        "The host is an output of an Infrastructure document, named by `ref` and `output`.",
    ),
];

/// The events `notifies[].on` accepts.
pub const NOTIFY_ON: &[Choice] = &[choice(
    NOTIFY_ON_CHANGED,
    "Fire when the notifying resource really changed. This is also what leaving \
     `on` out means.",
)];

const API_VERSION: Field = Field {
    name: "apiVersion",
    doc: "The version of the document language this document is written in.",
    required: true,
    ty: Type::Enum(API_VERSIONS),
    role: Role::None,
};

const KIND: Field = Field {
    name: "kind",
    doc: "What the document describes: `Infrastructure` provisions things, \
          `ResourceSet` configures something that exists.",
    required: true,
    ty: Type::Enum(KINDS),
    role: Role::None,
};

/// `metadata`.
pub const METADATA: Shape = Shape {
    name: "Metadata",
    doc: "The document's identity.",
    fields: &[
        text(
            "name",
            true,
            Role::DocumentName,
            "The document's name. It must be unique among documents of the same kind in \
             a workflow, and it is what `targetHost.ref` refers to.",
        ),
        Field {
            name: "labels",
            doc: "Names and values used to select documents with `-l/--selector`, as in \
                  `tier=web,env!=prod`. Labels are not passed to the target.",
            required: false,
            ty: Type::StringMap,
            role: Role::None,
        },
    ],
};

/// One entry of `dependsOn`.
pub const DEPENDENCY: Shape = Shape {
    name: "Dependency",
    doc: "A hard ordering constraint: the named resource is applied before this one.",
    fields: &[
        text(
            "resource",
            true,
            Role::ResourceRef,
            "The id of the resource that must be applied first.",
        ),
        Field {
            name: "ifMissing",
            doc: "What to do when a `when` condition has removed the named resource.",
            required: false,
            ty: Type::Enum(IF_MISSING),
            role: Role::None,
        },
        text(
            "when",
            false,
            Role::Condition,
            "A condition on this dependency alone. When it is false the entry is dropped and both resources stay.",
        ),
    ],
};

/// One entry of `notifies`.
pub const NOTIFICATION: Shape = Shape {
    name: "Notification",
    doc: "A trigger: when this resource changes, the named resource performs an action.",
    fields: &[
        text(
            "resource",
            true,
            Role::ResourceRef,
            "The id of the resource to notify.",
        ),
        Field {
            name: "on",
            doc: "The event that fires the notify.",
            required: false,
            ty: Type::Enum(NOTIFY_ON),
            role: Role::None,
        },
        text(
            "action",
            true,
            Role::NotifyAction,
            "What the notified resource does, such as `restart` or `reload`. Which \
             actions exist depends on the target and the notified resource's type.",
        ),
        Field {
            name: "ifMissing",
            doc: "What to do when a `when` condition has removed the named resource.",
            required: false,
            ty: Type::Enum(IF_MISSING),
            role: Role::None,
        },
        text(
            "when",
            false,
            Role::Condition,
            "A condition on this notify alone. When it is false the entry is dropped and both resources stay.",
        ),
    ],
};

/// One entry of `spec.resources`.
pub const RESOURCE: Shape = Shape {
    name: "Resource",
    doc: "One unit of desired state.",
    fields: &[
        text(
            "id",
            true,
            Role::ResourceId,
            "The resource's name within the document. `dependsOn`, `notifies` and \
             `${id.param}` references use it, so it must be unique.",
        ),
        text(
            "type",
            true,
            Role::ResourceType,
            "What kind of thing the resource is. The target decides which types exist.",
        ),
        text(
            "state",
            false,
            Role::ResourceState,
            "The state the resource should be left in, such as `present` or `running`. \
             Each type has a default, used when this is left out.",
        ),
        text(
            "when",
            false,
            Role::Condition,
            "A condition Meridian decides while compiling, from the values given with \
             `--data`. When it is false the resource is left out of the compiled \
             document. Example: `data.env == \"prod\" && defined(data.region)`.",
        ),
        text(
            "runtimeWhen",
            false,
            Role::RuntimeCondition,
            "A condition written in the tool's own language and checked on the node at \
             the moment of apply. Only a target that can express it accepts it.",
        ),
        Field {
            name: "params",
            doc: "The resource's settings. The target and the resource type decide which \
                  params exist.",
            required: false,
            ty: Type::Params,
            role: Role::ResourceParams,
        },
        Field {
            name: "dependsOn",
            doc: "Resources that must be applied before this one.",
            required: false,
            ty: Type::List(&DEPENDENCY),
            role: Role::None,
        },
        Field {
            name: "notifies",
            doc: "Resources to trigger when this one changes.",
            required: false,
            ty: Type::List(&NOTIFICATION),
            role: Role::None,
        },
    ],
};

/// One entry of `spec.outputs`.
pub const OUTPUT: Shape = Shape {
    name: "Output",
    doc: "A value this document hands to later documents in the workflow.",
    fields: &[
        text(
            "name",
            true,
            Role::OutputName,
            "The name later documents ask for the value by.",
        ),
        text(
            "value",
            false,
            Role::Expression,
            "Where the value comes from, usually a reference to something the tool \
             reports after applying, such as `${web_vm.public_ip}`.",
        ),
        text(
            "when",
            false,
            Role::Condition,
            "A condition on the output. When it is false the output is not declared.",
        ),
    ],
};

/// Where a secret comes from.
pub const SECRET_FROM: &[Choice] = &[
    choice("env", "An environment variable, named by `key`."),
    choice("file", "A file, named by `path`."),
    choice(
        "exec",
        "The output of a program, given by `command`. No shell reads it.",
    ),
    choice(
        "keyring",
        "A key in the Linux kernel keyring, found by its description, `key`.",
    ),
    choice(
        "systemd-creds",
        "A credential systemd passed to this unit, named by `key`.",
    ),
    choice(
        "secret-service",
        "An item in the freedesktop Secret Service (GNOME Keyring, KWallet), found by its \
         `attributes`.",
    ),
    choice(
        "keychain",
        "A generic password in the macOS keychain, found by `service` and `account`.",
    ),
];

const fn flag(name: &'static str, doc: &'static str) -> Field {
    Field {
        name,
        doc,
        required: false,
        ty: Type::Bool,
        role: Role::None,
    }
}

/// One entry of a secret's `fallback`: another place to look.
pub const SECRET_SOURCE: Shape = Shape {
    name: "SecretSource",
    doc: "Another place a secret is looked for, when the places before it have nothing.",
    fields: &[
        Field {
            name: "from",
            doc: "The provider the value is fetched from.",
            required: true,
            ty: Type::Enum(SECRET_FROM),
            role: Role::None,
        },
        text(
            "key",
            false,
            Role::None,
            "With `from: env`: the environment variable that holds the value. With \
             `from: keyring`: the description of the key. With `from: systemd-creds`: the \
             name of the credential.",
        ),
        text(
            "path",
            false,
            Role::None,
            "With `from: file`: the file that holds the value.",
        ),
        Field {
            name: "command",
            doc: "With `from: exec`: the program and its arguments, as a list. No shell \
                  reads them.",
            required: false,
            ty: Type::StringList,
            role: Role::None,
        },
        text(
            "service",
            false,
            Role::None,
            "With `from: keychain`: the service of the keychain item.",
        ),
        text(
            "account",
            false,
            Role::None,
            "With `from: keychain`: the account of the keychain item.",
        ),
        Field {
            name: "attributes",
            doc: "With `from: secret-service`: the attributes the item is found by. They \
                  are not secret.",
            required: false,
            ty: Type::StringMap,
            role: Role::None,
        },
        flag(
            "allowWideMode",
            "With `from: file`: read the file even though others can read it as well.",
        ),
        flag(
            "keepTrailingNewline",
            "With `from: file` and `from: systemd-creds`: keep the value's last line ending.",
        ),
    ],
};

/// One entry of `spec.secrets`.
pub const SECRET: Shape = Shape {
    name: "Secret",
    doc: "A secret's name and where it comes from. Its value is never written in a document.",
    fields: &[
        text(
            "name",
            true,
            Role::None,
            "The name the secret is read by, as `secret.<name>`.",
        ),
        Field {
            name: "from",
            doc: "The provider the value is fetched from.",
            required: true,
            ty: Type::Enum(SECRET_FROM),
            role: Role::None,
        },
        text(
            "key",
            false,
            Role::None,
            "With `from: env`: the environment variable that holds the value. With \
             `from: keyring`: the description of the key. With `from: systemd-creds`: the \
             name of the credential.",
        ),
        text(
            "path",
            false,
            Role::None,
            "With `from: file`: the file that holds the value.",
        ),
        Field {
            name: "command",
            doc: "With `from: exec`: the program and its arguments, as a list. No shell \
                  reads them.",
            required: false,
            ty: Type::StringList,
            role: Role::None,
        },
        text(
            "service",
            false,
            Role::None,
            "With `from: keychain`: the service of the keychain item.",
        ),
        text(
            "account",
            false,
            Role::None,
            "With `from: keychain`: the account of the keychain item.",
        ),
        Field {
            name: "attributes",
            doc: "With `from: secret-service`: the attributes the item is found by. They \
                  are not secret.",
            required: false,
            ty: Type::StringMap,
            role: Role::None,
        },
        flag(
            "allowWideMode",
            "With `from: file`: read the file even though others can read it as well.",
        ),
        flag(
            "keepTrailingNewline",
            "With `from: file` and `from: systemd-creds`: keep the value's last line ending.",
        ),
        flag(
            "optional",
            "A secret that cannot be found is `null` instead of an error.",
        ),
        Field {
            name: "fallback",
            doc: "The places to look next, in order, when the first has nothing or cannot \
                  be used on this machine.",
            required: false,
            ty: Type::List(&SECRET_SOURCE),
            role: Role::None,
        },
    ],
};

/// `spec.targetHost`.
pub const TARGET_HOST: Shape = Shape {
    name: "TargetHost",
    doc: "The host a ResourceSet configures.",
    fields: &[
        Field {
            name: "from",
            doc: "Where the host comes from.",
            required: true,
            ty: Type::Enum(TARGET_HOST_FROM),
            role: Role::None,
        },
        text(
            "host",
            false,
            Role::None,
            "The host's name or address. Required with `from: inline`.",
        ),
        text(
            "ref",
            false,
            Role::InfrastructureRef,
            "The `metadata.name` of the Infrastructure document that provides the host. \
             Required with `from: infrastructure`.",
        ),
        text(
            "output",
            false,
            Role::OutputRef,
            "The output of that document that holds the host. Required with \
             `from: infrastructure`.",
        ),
    ],
};

const TARGET: Field = text(
    "target",
    true,
    Role::Target,
    "The target plugin that compiles this document and runs its tool, such as \
     `ansible` or `terraform`.",
);

const TARGET_VERSION: Field = text(
    "targetVersion",
    false,
    Role::TargetVersion,
    "The version of the target's tool to compile for. Only a target that declares \
     versions accepts it.",
);

const RESOURCES: Field = Field {
    name: "resources",
    doc: "The resources that make up the document. At least one is required.",
    required: true,
    ty: Type::List(&RESOURCE),
    role: Role::None,
};

const SPEC_WHEN: Field = text(
    "when",
    false,
    Role::Condition,
    "A condition on the whole document. When it is false the document is skipped.",
);

const VARIABLES: Field = Field {
    name: "variables",
    doc: "Named values, worked out once per document and read as `var.<name>`.",
    required: false,
    ty: Type::Params,
    role: Role::None,
};

const CONDITIONS: Field = Field {
    name: "conditions",
    doc: "Named conditions, read as `cond.<name>` by any `when` in the document.",
    required: false,
    ty: Type::StringMap,
    role: Role::None,
};

const SECRETS: Field = Field {
    name: "secrets",
    doc: "The secrets the document reads, each a name and where it comes from, read as \
          `secret.<name>`.",
    required: false,
    ty: Type::List(&SECRET),
    role: Role::None,
};

/// Where an imported value comes from.
pub const IMPORT_FROM: &[Choice] = &[
    choice(
        IMPORT_FROM_ARTIFACT,
        "The text of a file another document compiles to, named by `ref` and `file`.",
    ),
    choice(
        IMPORT_FROM_OUTPUT,
        "A value another document's apply produced, named by `ref` and `output`.",
    ),
];

/// One entry of `spec.imports`.
pub const IMPORT: Shape = Shape {
    name: "Import",
    doc: "A value the document's tool is handed from another document of the workflow.",
    fields: &[
        text(
            "name",
            true,
            Role::None,
            "The name the value arrives under, as the tool's own variable or parameter.",
        ),
        Field {
            name: "from",
            doc: "Where the value comes from.",
            required: true,
            ty: Type::Enum(IMPORT_FROM),
            role: Role::None,
        },
        text(
            "ref",
            true,
            Role::None,
            "The `metadata.name` of the document the value comes from.",
        ),
        text(
            "file",
            false,
            Role::None,
            "With `from: artifact`: the compiled file whose text is the value.",
        ),
        text(
            "output",
            false,
            Role::None,
            "With `from: output`: the output whose value it is.",
        ),
        flag("sensitive", "Mask the value wherever Meridian prints."),
    ],
};

const IMPORTS: Field = Field {
    name: "imports",
    doc: "Values handed to the document's tool from other documents of the workflow.",
    required: false,
    ty: Type::List(&IMPORT),
    role: Role::None,
};

/// `spec` of an Infrastructure document.
pub const INFRASTRUCTURE_SPEC: Shape = Shape {
    name: "InfrastructureSpec",
    doc: "What an Infrastructure document provisions, and what it hands on.",
    fields: &[
        TARGET,
        TARGET_VERSION,
        SPEC_WHEN,
        VARIABLES,
        CONDITIONS,
        SECRETS,
        IMPORTS,
        RESOURCES,
        Field {
            name: "outputs",
            doc: "Values this document hands to later documents, such as a new \
                  machine's address.",
            required: false,
            ty: Type::List(&OUTPUT),
            role: Role::None,
        },
    ],
};

/// `spec` of a ResourceSet document.
pub const RESOURCE_SET_SPEC: Shape = Shape {
    name: "ResourceSetSpec",
    doc: "What a ResourceSet document configures, and where.",
    fields: &[
        TARGET,
        TARGET_VERSION,
        SPEC_WHEN,
        VARIABLES,
        CONDITIONS,
        SECRETS,
        Field {
            name: "targetHost",
            doc: "The host this document configures.",
            required: false,
            ty: Type::Object(&TARGET_HOST),
            role: Role::None,
        },
        IMPORTS,
        RESOURCES,
    ],
};

const METADATA_FIELD: Field = Field {
    name: "metadata",
    doc: "The document's identity: its name and labels.",
    required: true,
    ty: Type::Object(&METADATA),
    role: Role::None,
};

/// A whole `kind: Infrastructure` document.
pub const INFRASTRUCTURE: Shape = Shape {
    name: "Infrastructure",
    doc: "A document that provisions infrastructure.",
    fields: &[
        API_VERSION,
        KIND,
        METADATA_FIELD,
        Field {
            name: "spec",
            doc: "What the document provisions.",
            required: true,
            ty: Type::Object(&INFRASTRUCTURE_SPEC),
            role: Role::None,
        },
    ],
};

/// A whole `kind: ResourceSet` document.
pub const RESOURCE_SET: Shape = Shape {
    name: "ResourceSet",
    doc: "A document that configures something that already exists.",
    fields: &[
        API_VERSION,
        KIND,
        METADATA_FIELD,
        Field {
            name: "spec",
            doc: "What the document configures.",
            required: true,
            ty: Type::Object(&RESOURCE_SET_SPEC),
            role: Role::None,
        },
    ],
};

/// Every document kind, in the order `KINDS` lists them.
pub const DOCUMENTS: &[(Kind, &Shape)] = &[
    (Kind::Infrastructure, &INFRASTRUCTURE),
    (Kind::ResourceSet, &RESOURCE_SET),
];

/// The shape of a document of `kind`.
pub fn document(kind: Kind) -> &'static Shape {
    match kind {
        Kind::Infrastructure => &INFRASTRUCTURE,
        Kind::ResourceSet => &RESOURCE_SET,
    }
}

/// The kind a `kind:` value names.
pub fn kind_named(name: &str) -> Option<Kind> {
    DOCUMENTS
        .iter()
        .map(|(kind, _)| *kind)
        .find(|kind| kind.as_str() == name)
}

/// The shape a dotted path leads to inside `root`, and the field that led
/// there. List indexes are ignored, so `spec.resources[3].dependsOn` and
/// `spec.resources.dependsOn` find the same field.
///
/// Returns `None` once the path leaves the language: an unknown field, or
/// anything inside a free-form mapping such as `params`.
pub fn field_at(root: &'static Shape, path: &[&str]) -> Option<&'static Field> {
    let mut shape = root;
    let mut found = None;
    for (i, segment) in path.iter().enumerate() {
        let name = segment.split('[').next().unwrap_or(segment);
        let field = shape.field(name)?;
        found = Some(field);
        if i + 1 == path.len() {
            break;
        }
        shape = match &field.ty {
            Type::Object(next) | Type::List(next) => next,
            _ => return None,
        };
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What an editor offers for `apiVersion` is what the parser reads: the
    /// versions of the build's own table that still compile.
    #[test]
    fn the_api_versions_offered_are_the_ones_the_build_reads() {
        let readable: Vec<String> = crate::VersionTable::builtin()
            .versions()
            .iter()
            .filter(|v| !matches!(v.status, crate::Status::Removed { .. }))
            .map(|v| v.name())
            .collect();
        let offered: Vec<&str> = API_VERSIONS.iter().map(|c| c.value).collect();
        assert_eq!(offered, readable);
    }

    fn walk(shape: &'static Shape, seen: &mut Vec<&'static str>) {
        if seen.contains(&shape.name) {
            return;
        }
        seen.push(shape.name);
        assert!(!shape.doc.is_empty(), "{} has no doc", shape.name);
        for (i, field) in shape.fields.iter().enumerate() {
            assert!(
                !field.doc.is_empty(),
                "{}.{} has no doc",
                shape.name,
                field.name
            );
            assert!(
                !shape.fields[..i].iter().any(|f| f.name == field.name),
                "{}.{} is listed twice",
                shape.name,
                field.name
            );
            match &field.ty {
                Type::Object(next) | Type::List(next) => walk(next, seen),
                Type::Enum(choices) => assert!(!choices.is_empty()),
                _ => {}
            }
        }
    }

    #[test]
    fn every_shape_and_field_is_documented_and_named_once() {
        let mut seen = Vec::new();
        for (_, shape) in DOCUMENTS {
            walk(shape, &mut seen);
        }
        assert!(seen.contains(&"Resource"));
    }

    #[test]
    fn every_kind_has_a_shape_and_a_choice() {
        for choice in KINDS {
            let kind = kind_named(choice.value).expect("a listed kind has a shape");
            assert_eq!(document(kind).name, choice.value);
        }
        assert_eq!(KINDS.len(), DOCUMENTS.len());
    }

    #[test]
    fn a_path_finds_its_field_through_lists() {
        let field = field_at(
            &RESOURCE_SET,
            &["spec", "resources[2]", "dependsOn[0]", "resource"],
        )
        .expect("the path is in the language");
        assert_eq!(field.role, Role::ResourceRef);
        assert!(field_at(&RESOURCE_SET, &["spec", "resources", "params", "name"]).is_none());
        assert!(field_at(&RESOURCE_SET, &["spec", "outputs"]).is_none());
    }
}
