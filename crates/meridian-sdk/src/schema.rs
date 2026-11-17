//! What a target tells an editor about its resource types.
//!
//! A target already refuses a param or a state it does not take, but only
//! once a document is compiled. A schema says the same thing ahead of time,
//! so an editor can offer a type's params while the author is still typing
//! and explain each one. It is advice: `emit` remains the only judge of
//! whether a document compiles, and a schema must never promise something
//! `emit` refuses.
//!
//! A target that takes its tool's own types, such as `aws_instance`, cannot
//! list every param of every provider. It marks such a type `open`, which
//! tells an editor to accept any param and offer only what is listed.

/// The shape of one param's value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ParamKind {
    /// Not stated: any value.
    #[default]
    Any,
    String,
    Bool,
    Integer,
    Number,
    List,
    Map,
}

impl ParamKind {
    /// The name the kind travels under and is shown as.
    pub fn as_str(self) -> &'static str {
        match self {
            ParamKind::Any => "any",
            ParamKind::String => "string",
            ParamKind::Bool => "boolean",
            ParamKind::Integer => "integer",
            ParamKind::Number => "number",
            ParamKind::List => "list",
            ParamKind::Map => "mapping",
        }
    }

    /// The kind a name stands for. A name this build does not know is `Any`,
    /// so a newer plugin's kinds never make an older host refuse it.
    pub fn from_name(name: &str) -> ParamKind {
        match name {
            "string" => ParamKind::String,
            "boolean" => ParamKind::Bool,
            "integer" => ParamKind::Integer,
            "number" => ParamKind::Number,
            "list" => ParamKind::List,
            "mapping" => ParamKind::Map,
            _ => ParamKind::Any,
        }
    }
}

/// One param of a resource type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParamSchema {
    pub name: String,
    /// What the param means, for the person filling it in.
    pub description: String,
    pub kind: ParamKind,
    /// Whether the target refuses a resource that leaves it out.
    pub required: bool,
    /// The values it accepts, when they are a fixed set; empty otherwise.
    pub values: Vec<String>,
}

impl ParamSchema {
    pub fn new(name: &str, kind: ParamKind, description: &str) -> Self {
        ParamSchema {
            name: name.to_owned(),
            description: description.to_owned(),
            kind,
            required: false,
            values: Vec::new(),
        }
    }

    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    pub fn values(mut self, values: &[&str]) -> Self {
        self.values = values.iter().map(|v| (*v).to_owned()).collect();
        self
    }
}

/// One resource type of a target.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResourceSchema {
    /// The type as a document writes it. A name with `<` in it, such as
    /// `<provider>_<type>`, is a pattern and not a literal type.
    pub r#type: String,
    pub description: String,
    /// The states the type accepts. Empty means it takes no state.
    pub states: Vec<String>,
    /// The state assumed when a resource declares none.
    pub default_state: String,
    pub params: Vec<ParamSchema>,
    /// Whether params beyond the listed ones are accepted.
    pub open: bool,
    /// The notify actions the type can perform.
    pub actions: Vec<String>,
}

impl ResourceSchema {
    /// A type whose params are exactly the listed ones.
    pub fn closed(name: &str, description: &str) -> Self {
        ResourceSchema {
            r#type: name.to_owned(),
            description: description.to_owned(),
            ..ResourceSchema::default()
        }
    }

    /// A type that passes its params through to the tool, so any param may
    /// be valid.
    pub fn open(name: &str, description: &str) -> Self {
        ResourceSchema {
            open: true,
            ..ResourceSchema::closed(name, description)
        }
    }

    pub fn states(mut self, states: &[&str], default: &str) -> Self {
        self.states = states.iter().map(|s| (*s).to_owned()).collect();
        self.default_state = default.to_owned();
        self
    }

    pub fn actions(mut self, actions: &[&str]) -> Self {
        self.actions = actions.iter().map(|a| (*a).to_owned()).collect();
        self
    }

    pub fn param(mut self, param: ParamSchema) -> Self {
        self.params.push(param);
        self
    }

    /// Whether the type name is a pattern, such as `<provider>_<type>`.
    pub fn is_pattern(&self) -> bool {
        self.r#type.contains('<')
    }

    /// The param called `name`, if the type lists one.
    pub fn find_param(&self, name: &str) -> Option<&ParamSchema> {
        self.params.iter().find(|p| p.name == name)
    }
}

/// What each of the shared configuration management types is.
const SHARED_TYPES: &[(&str, &str)] = &[
    (
        "exec",
        "A command to run. The escape hatch for what no other type covers.",
    ),
    (
        "file",
        "A file or directory, with its content, owner and mode.",
    ),
    ("group", "A local group."),
    (
        "package",
        "A package installed through the system's package manager.",
    ),
    (
        "service",
        "A system service: whether it runs, and whether it starts at boot.",
    ),
    ("user", "A local user account."),
];

/// The params of the shared configuration management types: type, param,
/// kind, description. One table, so every target that takes these types
/// describes a param in the same words.
const SHARED_PARAMS: &[(&str, &str, ParamKind, &str)] = &[
    (
        "exec",
        "chdir",
        ParamKind::String,
        "The directory to run the command in.",
    ),
    (
        "exec",
        "command",
        ParamKind::String,
        "The command line to run.",
    ),
    (
        "exec",
        "creates",
        ParamKind::String,
        "A path the command creates. The command is skipped when the path already exists.",
    ),
    (
        "exec",
        "removes",
        ParamKind::String,
        "A path the command removes. The command is skipped when the path does not exist.",
    ),
    (
        "exec",
        "shell",
        ParamKind::Bool,
        "Run the command through a shell, so pipes and redirection work.",
    ),
    (
        "file",
        "append",
        ParamKind::Bool,
        "Add the content to the end of the file instead of replacing it.",
    ),
    (
        "file",
        "content",
        ParamKind::String,
        "The file's whole content.",
    ),
    (
        "file",
        "encoding",
        ParamKind::String,
        "How the content is encoded, such as `b64` or `gzip`.",
    ),
    (
        "file",
        "group",
        ParamKind::String,
        "The group that owns the file.",
    ),
    (
        "file",
        "mode",
        ParamKind::String,
        "The permissions, written as a quoted string such as \"0644\" so it is not read \
         as a number.",
    ),
    (
        "file",
        "owner",
        ParamKind::String,
        "The user that owns the file.",
    ),
    (
        "file",
        "path",
        ParamKind::String,
        "Where the file or directory is.",
    ),
    (
        "group",
        "gid",
        ParamKind::Integer,
        "The group's numeric id.",
    ),
    (
        "group",
        "members",
        ParamKind::List,
        "The users that belong to the group.",
    ),
    ("group", "name", ParamKind::String, "The group's name."),
    (
        "group",
        "system",
        ParamKind::Bool,
        "Create a system group, with an id from the system range.",
    ),
    ("package", "name", ParamKind::String, "The package's name."),
    (
        "package",
        "version",
        ParamKind::String,
        "The version to install.",
    ),
    (
        "service",
        "enabled",
        ParamKind::Bool,
        "Whether the service starts when the machine boots.",
    ),
    ("service", "name", ParamKind::String, "The service's name."),
    (
        "user",
        "gecos",
        ParamKind::String,
        "The account's full name or comment.",
    ),
    (
        "user",
        "groups",
        ParamKind::List,
        "The groups the user belongs to, beyond its primary group.",
    ),
    (
        "user",
        "home",
        ParamKind::String,
        "The user's home directory.",
    ),
    (
        "user",
        "lock_passwd",
        ParamKind::Bool,
        "Disable password login for the account.",
    ),
    ("user", "name", ParamKind::String, "The user's login name."),
    (
        "user",
        "primary_group",
        ParamKind::String,
        "The user's primary group.",
    ),
    (
        "user",
        "shell",
        ParamKind::String,
        "The user's login shell.",
    ),
    (
        "user",
        "ssh_authorized_keys",
        ParamKind::List,
        "The public keys allowed to log in as the user.",
    ),
    (
        "user",
        "sudo",
        ParamKind::Any,
        "The user's sudo rule, such as `ALL=(ALL) NOPASSWD:ALL`.",
    ),
    (
        "user",
        "system",
        ParamKind::Bool,
        "Create a system account, with an id from the system range.",
    ),
    ("user", "uid", ParamKind::Integer, "The user's numeric id."),
];

/// Describes one of the shared configuration management types (`package`,
/// `service`, `file`, `user`, `group`, `exec`) as one target takes it.
///
/// The target passes what it accepts, straight from the table its transform
/// stage checks a resource against; the descriptions and value kinds come
/// from here. A param this module has no words for is still listed, with no
/// description, so a target can never accept something an editor hides.
pub fn shared(
    name: &str,
    params: &[&str],
    required: &[&str],
    states: &[&str],
    default_state: &str,
    actions: &[&str],
) -> ResourceSchema {
    let description = SHARED_TYPES
        .iter()
        .find(|(t, _)| *t == name)
        .map(|(_, d)| *d)
        .unwrap_or_default();
    let mut schema = ResourceSchema::closed(name, description)
        .states(states, default_state)
        .actions(actions);
    let mut names: Vec<&str> = params.to_vec();
    names.sort_unstable();
    names.dedup();
    for param in names {
        let (kind, description) = SHARED_PARAMS
            .iter()
            .find(|(t, p, _, _)| *t == name && *p == param)
            .map(|(_, _, kind, d)| (*kind, *d))
            .unwrap_or((ParamKind::Any, ""));
        let mut param_schema = ParamSchema::new(param, kind, description);
        param_schema.required = required.contains(&param);
        schema.params.push(param_schema);
    }
    schema.states.sort();
    schema.states.dedup();
    schema.actions.sort();
    schema.actions.dedup();
    schema
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shared_type_is_described_from_the_one_table() {
        let schema = shared(
            "service",
            &["name", "enabled"],
            &["name"],
            &["stopped", "running"],
            "running",
            &["restart"],
        );
        assert!(!schema.open);
        assert_eq!(schema.states, ["running", "stopped"]);
        let names: Vec<&str> = schema.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["enabled", "name"]);
        let name = schema.find_param("name").unwrap();
        assert!(name.required && !name.description.is_empty());
        assert_eq!(schema.find_param("enabled").unwrap().kind, ParamKind::Bool);
    }

    #[test]
    fn a_param_without_words_is_still_listed() {
        let schema = shared("package", &["name", "flavour"], &[], &[], "", &[]);
        let odd = schema.find_param("flavour").unwrap();
        assert_eq!(odd.kind, ParamKind::Any);
        assert!(odd.description.is_empty());
    }

    #[test]
    fn a_kind_survives_its_name() {
        for kind in [
            ParamKind::Any,
            ParamKind::String,
            ParamKind::Bool,
            ParamKind::Integer,
            ParamKind::Number,
            ParamKind::List,
            ParamKind::Map,
        ] {
            assert_eq!(ParamKind::from_name(kind.as_str()), kind);
        }
        assert_eq!(ParamKind::from_name("tensor"), ParamKind::Any);
    }
}
