//! State and plan encryption, the part of a configuration only OpenTofu has.
//!
//! An `encryption` resource is passed through as OpenTofu's `encryption`
//! block. What is checked here is what OpenTofu would otherwise report only
//! at `tofu init`, on the machine that runs it and after the document has been
//! reviewed: a method that names a key provider nobody declared, a state that
//! names a method nobody declared, a block that protects nothing.
//!
//! Two things OpenTofu accepts without comment are warned about, because both
//! leave a secret in the clear beside an encrypted state: a passphrase written
//! into the document, and a state that is encrypted while the plan is not.

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::expr::{self, Part};
use meridian_sdk::rules::{self, Rule};
use meridian_sdk::{CompileError, Value, Warning};

use crate::{refuse_at, Block, Config, Kind, NAME};

/// The arguments an `encryption` block takes.
const ARGUMENTS: &[&str] = &[
    "key_provider",
    "method",
    "plan",
    "remote_state_data_sources",
    "state",
];

/// The method type that encrypts nothing. OpenTofu has it so that a state can
/// be moved into or out of encryption; it takes no keys.
const UNENCRYPTED: &str = "unencrypted";

struct Checker<'a> {
    block: &'a Block,
}

impl Checker<'_> {
    fn refuse(&self, rule: &Rule, msg: String) -> CompileError {
        refuse_at(&self.block.name, &self.block.pos, rule, msg)
    }

    /// The `<type>.<name>` of everything declared under `key_provider` or
    /// `method`, each of which is a map of types to a map of names.
    fn declared(&self, argument: &str) -> Result<BTreeMap<String, &Value>, CompileError> {
        let mut found = BTreeMap::new();
        let Some(types) = self.block.body.get(argument) else {
            return Ok(found);
        };
        let shape = || {
            self.refuse(
                &rules::INVALID_PARAM,
                format!(
                    "param {argument:?} must be a map of types to names to settings, such as \
                     {argument}: {{{example}: {{main: {{…}}}}}}",
                    example = if argument == "method" {
                        "aes_gcm"
                    } else {
                        "pbkdf2"
                    }
                ),
            )
        };
        for (kind, names) in types.as_map().ok_or_else(shape)? {
            for (name, settings) in names.as_map().ok_or_else(shape)? {
                if settings.as_map().is_none() {
                    return Err(shape());
                }
                found.insert(format!("{kind}.{name}"), settings);
            }
        }
        Ok(found)
    }

    /// Checks one `method = ${method.<type>.<name>}` or
    /// `keys = ${key_provider.<type>.<name>}` against what was declared.
    /// Returns what it names, or `None` for an expression too involved to
    /// follow, which is left for OpenTofu to judge.
    fn reference(
        &self,
        value: &Value,
        namespace: &str,
        declared: &BTreeMap<String, &Value>,
        whose: &str,
    ) -> Result<Option<String>, CompileError> {
        let example = if namespace == "method" {
            "${method.aes_gcm.main}"
        } else {
            "${key_provider.pbkdf2.main}"
        };
        let invalid = || {
            self.refuse(
                &rules::INVALID_PARAM,
                format!("{whose} must be a reference such as {example}"),
            )
        };
        let text = value.as_str().ok_or_else(invalid)?;
        let parts = expr::parse(text).map_err(|msg| self.refuse(&rules::INVALID_REFERENCE, msg))?;
        let [Part::Expr(expression)] = parts.as_slice() else {
            return Err(invalid());
        };
        let (head, rest) = expr::head(expression);
        if !expr::is_path(expression) {
            // OpenTofu reads a method as a fixed name, before it can work
            // anything out, so an expression there can never be right. Keys
            // are an ordinary argument and may be computed.
            if namespace == "method" {
                return Err(self.refuse(
                    &rules::INVALID_PARAM,
                    format!(
                        "{whose} must be one fixed reference such as {example}; OpenTofu \
                         chooses the method before it evaluates anything, so an expression \
                         cannot choose it"
                    ),
                ));
            }
            return Ok(None);
        }
        let named = rest.strip_prefix('.').unwrap_or(rest);
        if head != namespace || named.split('.').count() != 2 {
            return Err(invalid());
        }
        if !declared.contains_key(named) {
            let known = if declared.is_empty() {
                "none are declared".to_owned()
            } else {
                format!(
                    "declared: {}",
                    declared.keys().cloned().collect::<Vec<_>>().join(", ")
                )
            };
            let what = if namespace == "method" {
                "method"
            } else {
                "key provider"
            };
            return Err(self.refuse(
                &rules::INVALID_REFERENCE,
                format!("{whose} names the {what} {named:?}, which the block does not declare ({known})"),
            ));
        }
        Ok(Some(named.to_owned()))
    }

    /// Checks one target (`state`, `plan`, or a remote state data source) and
    /// records the methods it uses.
    fn target(
        &self,
        value: &Value,
        whose: &str,
        methods: &BTreeMap<String, &Value>,
        used: &mut BTreeSet<String>,
    ) -> Result<(), CompileError> {
        let Some(settings) = value.as_map() else {
            return Err(self.refuse(
                &rules::INVALID_PARAM,
                format!("param {whose:?} must be a map"),
            ));
        };
        let Some(method) = settings.get("method") else {
            return Err(self.refuse(
                &rules::MISSING_PARAM,
                format!(
                    "{whose} does not say which method encrypts it; add \
                     method: ${{method.<type>.<name>}}"
                ),
            ));
        };
        used.extend(self.reference(method, "method", methods, &format!("{whose}.method"))?);
        if let Some(fallback) = settings.get("fallback") {
            let Some(fallback) = fallback.as_map() else {
                return Err(self.refuse(
                    &rules::INVALID_PARAM,
                    format!("param \"{whose}.fallback\" must be a map"),
                ));
            };
            if let Some(method) = fallback.get("method") {
                used.extend(self.reference(
                    method,
                    "method",
                    methods,
                    &format!("{whose}.fallback.method"),
                )?);
            }
        }
        if let Some(enforced) = settings.get("enforced") {
            if !matches!(enforced, Value::Bool(_)) {
                return Err(self.refuse(
                    &rules::INVALID_PARAM,
                    format!("param \"{whose}.enforced\" must be true or false"),
                ));
            }
        }
        Ok(())
    }
}

/// Whether a target ends up encrypted: it names a method that is not the
/// `unencrypted` one.
fn encrypts(settings: Option<&Value>) -> bool {
    settings
        .and_then(Value::as_map)
        .and_then(|s| s.get("method"))
        .and_then(Value::as_str)
        .is_some_and(|m| !m.contains(&format!("method.{UNENCRYPTED}.")))
}

/// Checks the document's encryption block, if it has one.
pub(crate) fn check(config: &Config) -> Result<Vec<Warning>, CompileError> {
    let Some(block) = config.blocks.iter().find(|b| b.kind == Kind::Encryption) else {
        return Ok(Vec::new());
    };
    let checker = Checker { block };
    let warn = |msg: String| Warning {
        target: NAME.to_owned(),
        resource: block.name.clone(),
        msg,
    };
    let mut warnings = Vec::new();

    if let Some(unknown) = block.body.keys().find(|k| !ARGUMENTS.contains(&k.as_str())) {
        return Err(checker.refuse(
            &rules::INVALID_PARAM,
            format!(
                "param {unknown:?} is not part of an encryption block (accepted: {})",
                ARGUMENTS.join(", ")
            ),
        ));
    }

    let providers = checker.declared("key_provider")?;
    let methods = checker.declared("method")?;

    let mut used_providers = BTreeSet::new();
    for (name, settings) in &methods {
        let settings = settings.as_map().expect("declared() checked the shape");
        let unencrypted = name.starts_with(&format!("{UNENCRYPTED}."));
        match settings.get("keys") {
            Some(keys) => used_providers.extend(checker.reference(
                keys,
                "key_provider",
                &providers,
                &format!("method.{name}.keys"),
            )?),
            None if name.starts_with("aes_gcm.") => {
                return Err(checker.refuse(
                    &rules::MISSING_PARAM,
                    format!(
                        "method {name:?} has no keys; add keys: ${{key_provider.<type>.<name>}}"
                    ),
                ))
            }
            None => {}
        }
        if unencrypted && !settings.is_empty() {
            return Err(checker.refuse(
                &rules::INVALID_PARAM,
                format!("method {name:?} encrypts nothing and takes no settings"),
            ));
        }
    }

    let mut used_methods = BTreeSet::new();
    let mut targets = 0;
    for argument in ["state", "plan"] {
        if let Some(value) = block.body.get(argument) {
            targets += 1;
            checker.target(value, argument, &methods, &mut used_methods)?;
        }
    }
    if let Some(remote) = block.body.get("remote_state_data_sources") {
        let Some(remote) = remote.as_map() else {
            return Err(checker.refuse(
                &rules::INVALID_PARAM,
                "param \"remote_state_data_sources\" must be a map".to_owned(),
            ));
        };
        for (key, value) in remote {
            match key.as_str() {
                "default" => {
                    targets += 1;
                    checker.target(
                        value,
                        "remote_state_data_sources.default",
                        &methods,
                        &mut used_methods,
                    )?;
                }
                "remote_state_data_source" => {
                    let Some(sources) = value.as_map() else {
                        return Err(checker.refuse(
                            &rules::INVALID_PARAM,
                            "param \"remote_state_data_sources.remote_state_data_source\" must \
                             be a map of data source names to settings"
                                .to_owned(),
                        ));
                    };
                    for (source, value) in sources {
                        targets += 1;
                        checker.target(
                            value,
                            &format!("remote_state_data_sources.remote_state_data_source.{source}"),
                            &methods,
                            &mut used_methods,
                        )?;
                    }
                }
                other => {
                    return Err(checker.refuse(
                        &rules::INVALID_PARAM,
                        format!(
                            "param \"remote_state_data_sources.{other}\" is not part of an \
                             encryption block (accepted: default, remote_state_data_source)"
                        ),
                    ))
                }
            }
        }
    }
    if targets == 0 {
        return Err(checker.refuse(
            &rules::MISSING_PARAM,
            "the encryption block protects nothing: add state, plan or \
             remote_state_data_sources, each naming the method that encrypts it"
                .to_owned(),
        ));
    }

    // What is declared and never used is almost always a typing mistake in
    // the name of the one that was meant.
    for name in methods.keys().filter(|m| !used_methods.contains(*m)) {
        warnings.push(warn(format!(
            "method {name:?} is declared and encrypts nothing: no state, plan or remote state \
             names it"
        )));
    }
    for name in providers.keys().filter(|p| !used_providers.contains(*p)) {
        // A key provider can also feed another key provider, or be chosen by
        // an expression too involved to follow, so this is only said when
        // nothing in the block could be reading it.
        let reference = format!("key_provider.{name}");
        let read_elsewhere = providers
            .values()
            .chain(methods.values())
            .any(|settings| mentions(settings, &reference));
        if !read_elsewhere {
            warnings.push(warn(format!(
                "key provider {name:?} is declared and no method uses its keys"
            )));
        }
    }

    for (name, settings) in &providers {
        if !name.starts_with("pbkdf2.") {
            continue;
        }
        if let Some(Value::String(passphrase)) = settings.as_map().and_then(|s| s.get("passphrase"))
        {
            if !expr::has_expr(passphrase) {
                warnings.push(warn(format!(
                    "key provider {name:?} has its passphrase written in the document, so it \
                     is also in the compiled configuration beside the state it protects; \
                     declare a sensitive variable and use passphrase: ${{<its id>}}"
                )));
            }
        }
    }

    if encrypts(block.body.get("state")) && !encrypts(block.body.get("plan")) {
        warnings.push(warn(
            "the state is encrypted and the plan is not: a saved plan holds a copy of the \
             state, and Meridian saves one in the work directory on every run; add \
             plan: {method: …}"
                .to_owned(),
        ));
    }

    Ok(warnings)
}

/// Writes every `method` of a checked encryption block the way OpenTofu reads
/// it in the JSON syntax.
///
/// A document names a method as it names anything, `${method.aes_gcm.main}`.
/// OpenTofu takes a method as a fixed name and refuses a template there, so
/// the configuration carries the bare `method.aes_gcm.main`.
pub(crate) fn normalise(config: &mut Config) {
    fn target(value: &mut Value) {
        let Value::Map(settings) = value else {
            return;
        };
        if let Some(Value::String(method)) = settings.get_mut("method") {
            if let Some(inner) = method.strip_prefix("${").and_then(|m| m.strip_suffix('}')) {
                *method = inner.trim().to_owned();
            }
        }
        if let Some(fallback) = settings.get_mut("fallback") {
            target(fallback);
        }
    }

    let Some(block) = config
        .blocks
        .iter_mut()
        .find(|b| b.kind == Kind::Encryption)
    else {
        return;
    };
    for argument in ["state", "plan"] {
        if let Some(value) = block.body.get_mut(argument) {
            target(value);
        }
    }
    if let Some(Value::Map(remote)) = block.body.get_mut("remote_state_data_sources") {
        if let Some(default) = remote.get_mut("default") {
            target(default);
        }
        if let Some(Value::Map(sources)) = remote.get_mut("remote_state_data_source") {
            sources.values_mut().for_each(target);
        }
    }
}

/// Whether any string inside a value mentions `needle`.
fn mentions(value: &Value, needle: &str) -> bool {
    match value {
        Value::String(s) => s.contains(needle),
        Value::List(items) => items.iter().any(|v| mentions(v, needle)),
        Value::Map(map) => map.values().any(|v| mentions(v, needle)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meridian_sdk::{decode_value, Position};

    fn config(json: &str) -> Config {
        let Value::Map(body) = decode_value(json.as_bytes()).unwrap() else {
            panic!("the test body is a JSON object");
        };
        Config {
            blocks: vec![Block {
                kind: Kind::Encryption,
                r#type: String::new(),
                name: "state_encryption".into(),
                body,
                refs: BTreeSet::new(),
                pos: Position::default(),
            }],
            ..Config::default()
        }
    }

    const PROVIDER: &str =
        r#""key_provider":{"pbkdf2":{"main":{"passphrase":"${var.passphrase}"}}}"#;
    const METHOD: &str = r#""method":{"aes_gcm":{"main":{"keys":"${key_provider.pbkdf2.main}"}}}"#;

    fn refusal(json: &str) -> CompileError {
        check(&config(json)).expect_err("the block should be refused")
    }

    fn warnings(json: &str) -> Vec<String> {
        check(&config(json))
            .unwrap()
            .into_iter()
            .map(|w| w.msg)
            .collect()
    }

    #[test]
    fn a_document_without_encryption_is_left_alone() {
        assert!(check(&Config::default()).unwrap().is_empty());
    }

    #[test]
    fn a_complete_block_passes_without_a_word() {
        let json = format!(
            r#"{{{PROVIDER},{METHOD},
              "state":{{"method":"${{method.aes_gcm.main}}","enforced":true}},
              "plan":{{"method":"${{method.aes_gcm.main}}"}}}}"#
        );
        assert_eq!(warnings(&json), Vec::<String>::new());
    }

    #[test]
    fn a_state_cannot_name_a_method_nobody_declared() {
        let err = refusal(&format!(
            r#"{{{PROVIDER},{METHOD},"state":{{"method":"${{method.aes_gcm.mian}}"}}}}"#
        ));
        assert_eq!(err.rule, "invalid-reference");
        assert_eq!(err.resource, "state_encryption");
        assert_eq!(
            err.msg,
            "state.method names the method \"aes_gcm.mian\", which the block does not declare \
             (declared: aes_gcm.main)"
        );
    }

    #[test]
    fn a_method_cannot_take_keys_from_a_provider_nobody_declared() {
        let err = refusal(&format!(
            r#"{{{METHOD},"state":{{"method":"${{method.aes_gcm.main}}"}}}}"#
        ));
        assert_eq!(err.rule, "invalid-reference");
        assert_eq!(
            err.msg,
            "method.aes_gcm.main.keys names the key provider \"pbkdf2.main\", which the block \
             does not declare (none are declared)"
        );
    }

    #[test]
    fn a_reference_must_be_written_as_one() {
        let err = refusal(&format!(
            r#"{{{PROVIDER},{METHOD},"state":{{"method":"method.aes_gcm.main"}}}}"#
        ));
        assert_eq!(err.rule, "invalid-param");
        assert_eq!(
            err.msg,
            "state.method must be a reference such as ${method.aes_gcm.main}"
        );

        // The wrong namespace is the same mistake.
        let err = refusal(&format!(
            r#"{{{PROVIDER},{METHOD},"state":{{"method":"${{key_provider.pbkdf2.main}}"}}}}"#
        ));
        assert_eq!(err.rule, "invalid-param");

        let err = refusal(&format!(
            r#"{{{PROVIDER},{METHOD},"state":{{"method":7}}}}"#
        ));
        assert_eq!(err.rule, "invalid-param");
    }

    #[test]
    fn keys_may_be_computed_and_a_method_may_not() {
        let json = format!(
            r#"{{{PROVIDER},
              "method":{{"aes_gcm":{{"main":{{"keys":"${{var.rotate ? key_provider.pbkdf2.main : key_provider.pbkdf2.next}}"}}}}}},
              "state":{{"method":"${{method.aes_gcm.main}}"}},
              "plan":{{"method":"${{method.aes_gcm.main}}"}}}}"#
        );
        // The keys expression is too involved to follow, so the provider it
        // may use is not reported as unused.
        assert_eq!(warnings(&json), Vec::<String>::new());

        let err = refusal(&format!(
            r#"{{{PROVIDER},{METHOD},
              "state":{{"method":"${{var.encrypt ? method.aes_gcm.main : method.unencrypted.off}}"}}}}"#
        ));
        assert_eq!(err.rule, "invalid-param");
        assert!(
            err.msg.starts_with(
                "state.method must be one fixed reference such as ${method.aes_gcm.main}"
            ),
            "{}",
            err.msg
        );
    }

    #[test]
    fn a_method_is_written_as_the_fixed_name_opentofu_reads() {
        let json = format!(
            r#"{{{PROVIDER},"method":{{"aes_gcm":{{"main":{{"keys":"${{key_provider.pbkdf2.main}}"}}}},"unencrypted":{{"off":{{}}}}}},
              "state":{{"method":"${{method.aes_gcm.main}}","fallback":{{"method":"${{ method.unencrypted.off }}"}}}},
              "plan":{{"method":"${{method.aes_gcm.main}}"}},
              "remote_state_data_sources":{{
                 "default":{{"method":"${{method.aes_gcm.main}}"}},
                 "remote_state_data_source":{{"network":{{"method":"${{method.aes_gcm.main}}"}}}}}}}}"#
        );
        let mut config = config(&json);
        check(&config).unwrap();
        normalise(&mut config);
        let body = Value::Map(config.blocks[0].body.clone());
        let at = |path: &str| {
            path.split('.')
                .fold(&body, |v, key| &v.as_map().unwrap()[key])
                .as_str()
                .unwrap()
                .to_owned()
        };
        assert_eq!(at("state.method"), "method.aes_gcm.main");
        assert_eq!(at("state.fallback.method"), "method.unencrypted.off");
        assert_eq!(at("plan.method"), "method.aes_gcm.main");
        assert_eq!(
            at("remote_state_data_sources.default.method"),
            "method.aes_gcm.main"
        );
        assert_eq!(
            at("remote_state_data_sources.remote_state_data_source.network.method"),
            "method.aes_gcm.main"
        );
        // Keys are an ordinary argument and stay a template.
        assert_eq!(
            at("method.aes_gcm.main.keys"),
            "${key_provider.pbkdf2.main}"
        );
    }

    #[test]
    fn a_block_that_protects_nothing_is_refused() {
        let err = refusal(&format!("{{{PROVIDER},{METHOD}}}"));
        assert_eq!(err.rule, "missing-param");
        assert!(err.msg.starts_with("the encryption block protects nothing"));
    }

    #[test]
    fn a_target_must_name_its_method() {
        let err = refusal(&format!(
            r#"{{{PROVIDER},{METHOD},"state":{{"enforced":true}}}}"#
        ));
        assert_eq!(err.rule, "missing-param");
        assert_eq!(
            err.msg,
            "state does not say which method encrypts it; add method: ${method.<type>.<name>}"
        );

        let err = refusal(&format!(
            r#"{{{PROVIDER},{METHOD},"state":{{"method":"${{method.aes_gcm.main}}","enforced":"yes"}}}}"#
        ));
        assert_eq!(err.msg, "param \"state.enforced\" must be true or false");
    }

    #[test]
    fn aes_gcm_needs_keys_and_unencrypted_takes_none() {
        let err = refusal(
            r#"{"method":{"aes_gcm":{"main":{}}},"state":{"method":"${method.aes_gcm.main}"}}"#,
        );
        assert_eq!(err.rule, "missing-param");
        assert_eq!(
            err.msg,
            "method \"aes_gcm.main\" has no keys; add keys: ${key_provider.<type>.<name>}"
        );

        let err = refusal(
            r#"{"method":{"unencrypted":{"off":{"keys":"x"}}},"state":{"method":"${method.unencrypted.off}"}}"#,
        );
        assert_eq!(err.rule, "invalid-param");
    }

    #[test]
    fn an_argument_that_is_not_encryptions_is_refused() {
        let err = refusal(&format!(
            r#"{{{PROVIDER},{METHOD},"state":{{"method":"${{method.aes_gcm.main}}"}},"statefile":{{}}}}"#
        ));
        assert_eq!(err.rule, "invalid-param");
        assert_eq!(
            err.msg,
            "param \"statefile\" is not part of an encryption block (accepted: key_provider, \
             method, plan, remote_state_data_sources, state)"
        );

        let err = refusal(r#"{"key_provider":{"pbkdf2":"main"},"state":{}}"#);
        assert_eq!(err.rule, "invalid-param");
        assert!(err
            .msg
            .starts_with("param \"key_provider\" must be a map of types"));
    }

    #[test]
    fn a_fallback_is_checked_like_the_method_it_replaces() {
        let migrating = format!(
            r#"{{{PROVIDER},"method":{{"aes_gcm":{{"main":{{"keys":"${{key_provider.pbkdf2.main}}"}}}},"unencrypted":{{"off":{{}}}}}},
              "state":{{"method":"${{method.aes_gcm.main}}","fallback":{{"method":"${{method.unencrypted.off}}"}}}},
              "plan":{{"method":"${{method.aes_gcm.main}}","fallback":{{"method":"${{method.unencrypted.off}}"}}}}}}"#
        );
        assert_eq!(warnings(&migrating), Vec::<String>::new());

        let err = refusal(&format!(
            r#"{{{PROVIDER},{METHOD},"state":{{"method":"${{method.aes_gcm.main}}","fallback":{{"method":"${{method.unencrypted.off}}"}}}}}}"#
        ));
        assert_eq!(
            err.msg,
            "state.fallback.method names the method \"unencrypted.off\", which the block does \
             not declare (declared: aes_gcm.main)"
        );
    }

    #[test]
    fn remote_state_data_sources_are_checked_too() {
        let json = format!(
            r#"{{{PROVIDER},{METHOD},"remote_state_data_sources":{{
                 "default":{{"method":"${{method.aes_gcm.main}}"}},
                 "remote_state_data_source":{{"network":{{"method":"${{method.aes_gcm.main}}"}}}}}}}}"#
        );
        assert_eq!(warnings(&json), Vec::<String>::new());

        let err = refusal(&format!(
            r#"{{{PROVIDER},{METHOD},"remote_state_data_sources":{{
                 "remote_state_data_source":{{"network":{{"method":"${{method.aes_gcm.other}}"}}}}}}}}"#
        ));
        assert!(
            err.msg.starts_with(
                "remote_state_data_sources.remote_state_data_source.network.method names"
            ),
            "{}",
            err.msg
        );

        let err = refusal(&format!(
            r#"{{{PROVIDER},{METHOD},"remote_state_data_sources":{{"defualt":{{}}}}}}"#
        ));
        assert_eq!(err.rule, "invalid-param");
    }

    #[test]
    fn a_passphrase_in_the_document_is_warned_about() {
        let json = format!(
            r#"{{"key_provider":{{"pbkdf2":{{"main":{{"passphrase":"correct-horse-battery-staple"}}}}}},{METHOD},
              "state":{{"method":"${{method.aes_gcm.main}}"}},"plan":{{"method":"${{method.aes_gcm.main}}"}}}}"#
        );
        let warnings = warnings(&json);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].starts_with(
            "key provider \"pbkdf2.main\" has its passphrase written in the document"
        ));
    }

    #[test]
    fn an_encrypted_state_with_a_plain_plan_is_warned_about() {
        let json =
            format!(r#"{{{PROVIDER},{METHOD},"state":{{"method":"${{method.aes_gcm.main}}"}}}}"#);
        let warnings = warnings(&json);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].starts_with("the state is encrypted and the plan is not"));
    }

    #[test]
    fn what_is_declared_and_never_used_is_warned_about() {
        let json = r#"{"key_provider":{"pbkdf2":{"main":{"passphrase":"${var.p}"},"spare":{"passphrase":"${var.q}"}}},
              "method":{"aes_gcm":{"main":{"keys":"${key_provider.pbkdf2.main}"},"old":{"keys":"${key_provider.pbkdf2.main}"}}},
              "state":{"method":"${method.aes_gcm.main}"},"plan":{"method":"${method.aes_gcm.main}"}}"#;
        assert_eq!(
            warnings(json),
            [
                "method \"aes_gcm.old\" is declared and encrypts nothing: no state, plan or \
                 remote state names it",
                "key provider \"pbkdf2.spare\" is declared and no method uses its keys",
            ]
        );
    }
}
