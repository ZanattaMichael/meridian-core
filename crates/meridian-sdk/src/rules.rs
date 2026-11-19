//! The registry of refusal rules.
//!
//! A [`CompileError`](crate::CompileError) carries a *rule*: a fixed name for
//! the mistake it refuses, which policy checks, editors, tests and scripts
//! key off. A name only works as a key when it is the same everywhere and
//! does not change by accident, so a rule is not a string written at the call
//! site: it is one of the constants here, or one a plugin declares in its own
//! table and reports through [`Emitter::rules`](crate::Emitter::rules).
//!
//! The rules here are *general*: the same mistake has the same name on every
//! target, however that target finds it. A plugin uses them wherever one fits,
//! and declares a rule of its own only for a mistake that exists on one tool,
//! such as Salt needing a shell for a command. The reference page
//! `docs/reference/refusal-rules.md` is generated from both.
//!
//! A rule's `meaning` says what the mistake is, and its `hint` what to write
//! instead, in general terms; the refusal's own message names the resource,
//! the value and what that target accepts.

/// One rule a refusal can carry: a stable name, what it refuses, and what to
/// do about it. The fields are static so a plugin can keep its own rules in a
/// `const` table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    /// Lower-case words joined by hyphens, such as `unsupported-param`.
    pub name: &'static str,
    /// What the mistake is, in one sentence.
    pub meaning: &'static str,
    /// What to write instead, in one sentence; empty when the message is
    /// the only guide.
    pub hint: &'static str,
}

impl Rule {
    /// A rule, for a plugin's own table.
    pub const fn new(name: &'static str, meaning: &'static str, hint: &'static str) -> Self {
        Rule {
            name,
            meaning,
            hint,
        }
    }
}

/// A rule as a plugin describes it over the wire, with owned text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DescribedRule {
    pub name: String,
    pub meaning: String,
    pub hint: String,
}

impl From<&Rule> for DescribedRule {
    fn from(rule: &Rule) -> Self {
        DescribedRule {
            name: rule.name.to_owned(),
            meaning: rule.meaning.to_owned(),
            hint: rule.hint.to_owned(),
        }
    }
}

macro_rules! general {
    ($( $(#[$doc:meta])* $konst:ident = ($name:literal, $meaning:literal, $hint:literal); )*) => {
        $(
            $(#[$doc])*
            pub const $konst: Rule = Rule::new($name, $meaning, $hint);
        )*
        /// Every general rule, in the order the reference page lists them.
        pub const GENERAL: &[Rule] = &[$($konst),*];
    };
}

general! {
    /// `type` is not one the target maps.
    UNSUPPORTED_RESOURCE_TYPE = (
        "unsupported-resource-type",
        "the resource's `type` is not one this target maps",
        "use a type from the target's resource types; `meridian plugins` lists them"
    );
    /// The state is not one the target can express for this type.
    UNSUPPORTED_STATE = (
        "unsupported-state",
        "the resource's `state` is not one this target can express for its type",
        "use a state the message lists; the target's page says what each does"
    );
    /// A param the target does not take for this type.
    UNSUPPORTED_PARAM = (
        "unsupported-param",
        "a param this target does not take for the resource's type",
        "remove the param, or use the name the message lists"
    );
    /// A param the type needs is not given.
    MISSING_PARAM = (
        "missing-param",
        "a param the resource's type needs is not given",
        "add the param the message names"
    );
    /// A param is given with no value where one is needed.
    EMPTY_PARAM = (
        "empty-param",
        "a param is given with no value, where the tool needs one",
        "give the param a value, or remove it"
    );
    /// A param's value is of the wrong kind or form.
    INVALID_PARAM = (
        "invalid-param",
        "a param's value is not the kind or form the resource's type takes",
        "write the value as the message says; quote a mode such as \"0644\""
    );
    /// A param that means nothing in the given state.
    PARAM_CONFLICTS_WITH_STATE = (
        "param-conflicts-with-state",
        "a param that means nothing in the resource's `state`, such as `content` on a directory",
        "remove the param, or change the state"
    );
    /// The resource's id or a name in it cannot be a name in the tool.
    INVALID_NAME = (
        "invalid-name",
        "the resource's id, or a name it gives, cannot be a name in the tool",
        "use a name of the form the message gives"
    );
    /// Two resources would occupy one place in the tool.
    DUPLICATE_RESOURCE = (
        "duplicate-resource",
        "two resources would be one thing to the tool: the same name, path or title, or a second resource of a type a document has one of",
        "keep one of the two, or give each its own name"
    );
    /// A `${...}` reference that cannot be followed.
    INVALID_REFERENCE = (
        "invalid-reference",
        "a `${...}` reference names a resource or value the document does not declare, reads what cannot be read where it stands, or is not well formed",
        "reference a resource of this document and a value it declares; see the references page"
    );
    /// `dependsOn` names a resource the document does not declare.
    UNKNOWN_DEPENDENCY = (
        "unknown-dependency",
        "`dependsOn` names a resource the document does not declare",
        "name a resource of this document"
    );
    /// The target cannot express this ordering.
    UNSUPPORTED_DEPENDENCY = (
        "unsupported-dependency",
        "an ordering between two resources that this target cannot express",
        "remove the `dependsOn`, or order the resources as the message says"
    );
    /// The dependencies form a cycle.
    DEPENDENCY_CYCLE = (
        "dependency-cycle",
        "the resources' dependencies form a cycle",
        "remove one `dependsOn` from the cycle"
    );
    /// The order the core handed over cannot be kept by the target.
    ORDERING_VIOLATED = (
        "ordering-violated",
        "the apply order the core produced puts a resource before its dependency, which this target cannot keep",
        "this is a defect in Meridian, not in the document: report it"
    );
    /// A `notifies` edge names a resource the document does not declare.
    UNKNOWN_NOTIFY_TARGET = (
        "unknown-notify-target",
        "a `notifies` names a resource the document does not declare, at either end",
        "name a resource of this document"
    );
    /// The target has no way to react to a change.
    UNSUPPORTED_NOTIFY = (
        "unsupported-notify",
        "the target has no way to react to a change, so `notifies` cannot be expressed",
        "remove the `notifies`; see the capability matrix"
    );
    /// The notify's action is not one the target can run.
    UNSUPPORTED_ACTION = (
        "unsupported-action",
        "a `notifies` action this target cannot run",
        "use an action the message lists"
    );
    /// `runtimeWhen` on a target that has none.
    UNSUPPORTED_RUNTIME_CONDITION = (
        "unsupported-runtime-condition",
        "a `runtimeWhen` on a target whose tool checks no condition on the node",
        "use a compile-time `when`, or remove the condition; see the capability matrix"
    );
    /// `adopt` on a target that cannot take an existing thing under
    /// management.
    UNSUPPORTED_ADOPT = (
        "unsupported-adopt",
        "an `adopt` on a target that cannot take a thing that already exists under management",
        "remove `adopt`, or use a target that adopts; see the capability matrix"
    );
    /// `adopt` on a resource of a kind its tool cannot import.
    INVALID_ADOPT = (
        "invalid-adopt",
        "an `adopt` on a resource that its tool cannot import, such as a data source or a provider",
        "put `adopt` on a resource the tool manages, as the target's page describes"
    );
    /// A `runtimeWhen` the target cannot read.
    INVALID_RUNTIME_CONDITION = (
        "invalid-runtime-condition",
        "a `runtimeWhen` this target cannot turn into its tool's own test",
        "write the condition as the target's page describes"
    );
    /// Two outputs with one name.
    DUPLICATE_OUTPUT = (
        "duplicate-output",
        "two outputs of the document have one name",
        "give each output its own name"
    );
    /// An output the target cannot provide.
    UNSUPPORTED_OUTPUT = (
        "unsupported-output",
        "an output this target cannot report, or a value it cannot read into one",
        "remove the output, or read a value the message says the tool reports"
    );
    /// A secret where the target cannot keep it secret.
    UNSUPPORTED_SECRET_PLACE = (
        "unsupported-secret-place",
        "a secret in a param this target would write into a file or a command line as given",
        "put the secret where the target's page says it stays secret"
    );
    /// A credential the target cannot use.
    UNSUPPORTED_CREDENTIAL = (
        "unsupported-credential",
        "the credential `targetHost.credential` names is of a kind this target cannot reach its host with",
        "use a credential kind the target's page lists"
    );
    /// A `spec.settings` key the target does not take.
    UNSUPPORTED_SETTING = (
        "unsupported-setting",
        "a `spec.settings` key this target does not read",
        "remove the key, or use one the message lists"
    );
    /// A `spec.settings` value the target cannot use.
    INVALID_SETTING = (
        "invalid-setting",
        "a `spec.settings` value that is not what the key takes",
        "write the value as the target's page describes"
    );
    /// Two places say different things about one setting.
    CONFLICTING_SETTINGS = (
        "conflicting-settings",
        "one setting is given in two places with different values, such as `spec.settings` and `spec.targetVersion`",
        "say it once"
    );
    /// A name or text Meridian keeps for itself.
    RESERVED_NAME = (
        "reserved-name",
        "a name, key or text that Meridian itself uses on this target, such as the prefix `meridian_secret_`",
        "choose another name"
    );
    /// `targetHost.host` cannot be written into the tool's files.
    INVALID_HOST = (
        "invalid-host",
        "`targetHost.host` holds characters the target cannot write into its tool's files",
        "give a host name or address, with nothing but letters, digits, dots, hyphens and colons"
    );
    /// The target needs a host and the document names none.
    MISSING_HOST = (
        "missing-host",
        "the document names no `targetHost`, and this target cannot compile without one",
        "set `spec.targetHost.host`"
    );
    /// The document compiles to nothing.
    EMPTY_DOCUMENT = (
        "empty-document",
        "the document holds nothing the tool would act on, so there is nothing to compile",
        "add a resource"
    );
    /// A resource the target needs in every document is not there.
    MISSING_RESOURCE = (
        "missing-resource",
        "a resource this target needs in every document is not there, such as a `deployment`",
        "add the resource the message names; the target's page says what it takes"
    );
    /// A path the target cannot use.
    INVALID_PATH = (
        "invalid-path",
        "a path this target cannot use: relative where an absolute one is needed, leaving the directory it must stay in, or holding characters the tool cannot take",
        "write the path as the message says"
    );
}

/// The general rule of this name, if there is one.
pub fn general(name: &str) -> Option<&'static Rule> {
    GENERAL.iter().find(|r| r.name == name)
}

/// Whether a name is lower-case words joined by hyphens, which every rule's
/// name must be.
pub fn well_formed(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn general_rules_are_distinct_and_well_formed() {
        let mut seen = BTreeSet::new();
        for rule in GENERAL {
            assert!(well_formed(rule.name), "{:?}", rule.name);
            assert!(seen.insert(rule.name), "{:?} is listed twice", rule.name);
            assert!(!rule.meaning.is_empty(), "{:?} has no meaning", rule.name);
            assert!(!rule.hint.is_empty(), "{:?} has no hint", rule.name);
        }
    }

    #[test]
    fn a_general_rule_is_found_by_name() {
        assert_eq!(general("unsupported-param"), Some(&UNSUPPORTED_PARAM));
        assert_eq!(general("unknown-param"), None);
    }

    #[test]
    fn well_formed_names() {
        for name in ["a", "unsupported-param", "rule-2"] {
            assert!(well_formed(name), "{name:?}");
        }
        for name in ["", "-a", "a-", "a--b", "Unsupported", "a_b", "a b"] {
            assert!(!well_formed(name), "{name:?}");
        }
    }
}
