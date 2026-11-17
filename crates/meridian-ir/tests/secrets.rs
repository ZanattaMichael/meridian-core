//! `spec.secrets`: how a secret is declared, where it may be read, and what
//! a resolved document holds of one, which is never its value.

use meridian_ir::{
    parse, resolve, resolve_with, Bundle, Data, Document, Resolution, Secrets, Value,
};
use meridian_secret::SecretValue;

const PASSWORD: &str = "p@ss/w0rd-9fXw";

fn secret(text: &str) -> SecretValue {
    SecretValue::from_reader(text.as_bytes(), 4096).unwrap()
}

/// A ResourceSet whose `spec` is `spec`, indented by two spaces.
fn resource_set(spec: &str) -> String {
    let body: String = spec.lines().map(|l| format!("  {l}\n")).collect();
    format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata: {{name: site}}\nspec:\n  target: \
         ansible\n  targetHost: {{from: inline, host: h}}\n{body}"
    )
}

fn infrastructure(spec: &str) -> String {
    let body: String = spec.lines().map(|l| format!("  {l}\n")).collect();
    format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata: {{name: net}}\nspec:\n  target: \
         terraform\n{body}"
    )
}

fn document(text: &str) -> Document {
    parse("doc.yaml", text.as_bytes())
        .unwrap_or_else(|e| panic!("{e}\n{text}"))
        .remove(0)
}

fn problems(text: &str) -> Vec<String> {
    match Bundle::new([document(text)]).validate() {
        Ok(()) => Vec::new(),
        Err(errors) => errors.to_string().lines().map(str::to_owned).collect(),
    }
}

fn valid(text: &str) {
    let found = problems(text);
    assert!(found.is_empty(), "{found:#?}\n{text}");
}

/// Asserts `validate` reports exactly one problem, holding every part.
fn refused(text: &str, parts: &[&str]) {
    let found = problems(text);
    assert_eq!(found.len(), 1, "{found:#?}\n{text}");
    for part in parts {
        assert!(found[0].contains(part), "{:?} lacks {part:?}", found[0]);
    }
}

const DECLARED: &str = "secrets:
  - name: db_password
    from: env
    key: DB_PASSWORD
  - name: tls_key
    from: file
    path: /run/secrets/site.key
    allowWideMode: true
    keepTrailingNewline: true
  - name: token
    from: exec
    command: [pass, show, registry]
    optional: true
";

fn document_declaring(rest: &str) -> String {
    resource_set(&format!("{DECLARED}{rest}"))
}

/// The document's secrets, with `db_password` and `tls_key` fetched and the
/// optional `token` not found.
fn fetched(doc: &Document) -> Secrets {
    let mut secrets = Secrets::declared(doc);
    secrets.hold("db_password", secret(PASSWORD));
    secrets.hold("tls_key", secret("-----KEY-----\n"));
    secrets.absent("token");
    secrets
}

fn kept(resolution: Resolution) -> Document {
    match resolution {
        Resolution::Kept { document, .. } => *document,
        Resolution::Skipped { condition } => panic!("skipped by {condition}"),
    }
}

fn param<'d>(doc: &'d Document, id: &str, name: &str) -> &'d Value {
    &doc.resources()
        .iter()
        .find(|r| r.id == id)
        .unwrap_or_else(|| panic!("no resource {id}"))
        .params[name]
}

fn is(secrets: &Secrets, value: &Value, want: &str) -> bool {
    let Value::Secret(at) = value else {
        panic!("not a secret: {value:?}");
    };
    secrets
        .table()
        .with(*at, |held| held.expose(|bytes| bytes == want.as_bytes()))
        .unwrap()
}

// ---- declaring ---------------------------------------------------------

#[test]
fn a_secret_is_declared_with_where_it_comes_from() {
    let doc = document(&document_declaring(
        "resources:\n  - id: x\n    type: file\n",
    ));
    let secrets = doc.secrets();
    assert_eq!(secrets.len(), 3);
    assert_eq!(
        (
            secrets[0].name.as_str(),
            secrets[0].from.as_str(),
            secrets[0].key.as_str()
        ),
        ("db_password", "env", "DB_PASSWORD")
    );
    assert!(!secrets[0].optional);
    assert_eq!(secrets[1].path, "/run/secrets/site.key");
    assert!(secrets[1].allow_wide_mode && secrets[1].keep_trailing_newline);
    assert_eq!(secrets[2].command, ["pass", "show", "registry"]);
    assert!(secrets[2].optional);
    valid(&document_declaring(
        "resources:\n  - id: x\n    type: file\n",
    ));
    // Both kinds of document take them.
    valid(&infrastructure(&format!(
        "{DECLARED}resources:\n  - id: x\n    type: file\n"
    )));
}

#[test]
fn a_declaration_that_is_incomplete_or_mixed_up_is_refused() {
    for (entry, parts) in [
        (
            "{from: env, key: K}",
            vec!["spec.secrets[0].name", "a secret needs a name"],
        ),
        (
            "{name: 9lives, from: env, key: K}",
            vec!["spec.secrets[0].name", "is not a usable name"],
        ),
        (
            "{name: has-dash, from: env, key: K}",
            vec!["spec.secrets[0].name", "is not a usable name"],
        ),
        (
            "{name: a}",
            vec!["spec.secrets[0].from", "from is one of env, exec, file"],
        ),
        (
            "{name: a, from: vault, path: secret/x}",
            vec![
                "spec.secrets[0].from",
                "\"vault\" is not a provider this version has",
                "env, exec, file",
            ],
        ),
        (
            "{name: a, from: env}",
            vec![
                "spec.secrets[0].key",
                "from: env needs key, the environment variable",
            ],
        ),
        (
            "{name: a, from: file}",
            vec!["spec.secrets[0].path", "from: file needs path"],
        ),
        (
            "{name: a, from: exec}",
            vec!["spec.secrets[0].command", "from: exec needs command"],
        ),
        (
            "{name: a, from: exec, command: ['']}",
            vec![
                "spec.secrets[0].command",
                "the first item of command is the program",
            ],
        ),
        (
            "{name: a, from: env, key: K, path: /x}",
            vec!["spec.secrets[0].path", "path means nothing to from: env"],
        ),
        (
            "{name: a, from: env, key: K, allowWideMode: true}",
            vec![
                "spec.secrets[0].allowWideMode",
                "means nothing to from: env",
            ],
        ),
        (
            "{name: a, from: file, path: /x, command: [cat]}",
            vec![
                "spec.secrets[0].command",
                "command means nothing to from: file",
            ],
        ),
        (
            "{name: a, from: exec, command: [cat], key: K}",
            vec!["spec.secrets[0].key", "key means nothing to from: exec"],
        ),
        (
            "{name: a, from: exec, command: [cat], keepTrailingNewline: true}",
            vec![
                "spec.secrets[0].keepTrailingNewline",
                "means nothing to from: exec",
            ],
        ),
    ] {
        refused(
            &resource_set(&format!(
                "secrets:\n  - {entry}\nresources:\n  - id: x\n    type: file\n"
            )),
            &parts,
        );
    }
}

#[test]
fn a_secret_declared_twice_is_refused() {
    refused(
        &resource_set(
            "secrets:\n  - {name: a, from: env, key: A}\n  - {name: a, from: env, key: B}\n\
             resources:\n    - id: x\n      type: file\n",
        ),
        &["spec.secrets[1].name", "the secret \"a\" is declared twice"],
    );
}

#[test]
fn a_field_that_is_not_one_or_has_the_wrong_type_is_refused_when_read() {
    let err = parse(
        "doc.yaml",
        resource_set("secrets:\n  - {name: a, from: env, key: K, value: hunter2}\nresources:\n  - id: x\n    type: file\n")
            .as_bytes(),
    )
    .unwrap_err()
    .to_string();
    // There is no field to write a value in, and the error does not repeat
    // what was written there.
    assert!(
        err.contains("spec.secrets[0]") && err.contains("value"),
        "{err}"
    );
    assert!(!err.contains("hunter2"), "{err}");

    let err = parse(
        "doc.yaml",
        resource_set("secrets:\n  - {name: a, from: env, key: K, optional: maybe}\nresources:\n  - id: x\n    type: file\n")
            .as_bytes(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("spec.secrets[0].optional") && err.contains("expected true or false"),
        "{err}"
    );

    let err = parse(
        "doc.yaml",
        resource_set("secrets: {a: {from: env}}\nresources:\n  - id: x\n    type: file\n")
            .as_bytes(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("spec.secrets") && err.contains("expected a list"),
        "{err}"
    );
}

#[test]
fn where_a_secret_comes_from_cannot_be_interpolated() {
    let open = concat!("$", "{{");
    for (field, entry) in [
        (
            "key",
            format!("{{name: a, from: env, key: \"{open} data.key }}}}\"}}"),
        ),
        (
            "path",
            format!("{{name: a, from: file, path: \"/run/{open} data.env }}}}.key\"}}"),
        ),
        (
            "command",
            format!("{{name: a, from: exec, command: [pass, \"{open} data.entry }}}}\"]}}"),
        ),
    ] {
        refused(
            &resource_set(&format!(
                "secrets:\n  - {entry}\nresources:\n  - id: x\n    type: file\n"
            )),
            &[
                &format!("spec.secrets[0].{field}"),
                "where a secret comes from is written out",
            ],
        );
    }
}

#[test]
fn a_secret_can_come_from_a_store_of_the_operating_system() {
    let text = resource_set(
        "secrets:
  - name: deploy
    from: keyring
    key: meridian:deploy
  - name: unit
    from: systemd-creds
    key: deploy-token
    keepTrailingNewline: true
  - name: desktop
    from: secret-service
    attributes: {app: meridian, name: deploy}
  - name: mac
    from: keychain
    service: deploy
    account: meridian
resources:
  - id: x
    type: file
",
    );
    valid(&text);
    let doc = document(&text);
    let secrets = doc.secrets();
    assert_eq!(
        secrets
            .iter()
            .map(|s| (s.name.as_str(), s.from.as_str()))
            .collect::<Vec<_>>(),
        [
            ("deploy", "keyring"),
            ("unit", "systemd-creds"),
            ("desktop", "secret-service"),
            ("mac", "keychain")
        ]
    );
    assert_eq!(secrets[0].key, "meridian:deploy");
    assert!(secrets[1].keep_trailing_newline);
    assert_eq!(
        secrets[2]
            .attributes
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>(),
        ["app=meridian", "name=deploy"]
    );
    assert_eq!(
        (secrets[3].service.as_str(), secrets[3].account.as_str()),
        ("deploy", "meridian")
    );
    // Each is one place, and that place is what the secrets crate is given.
    for secret in secrets {
        assert!(secret.fallback.is_empty());
        assert_eq!(secret.sources(), [secret.source()]);
        assert_eq!(secret.source().from, secret.from);
    }
    assert_eq!(
        secrets[3].source().place(),
        "the keychain, service deploy and account meridian"
    );
}

const WITH_FALLBACK: &str = "secrets:
  - name: deploy
    from: secret-service
    attributes: {app: meridian, name: deploy}
    optional: true
    fallback:
      - from: keyring
        key: meridian:deploy
      - from: file
        path: /run/secrets/deploy
        allowWideMode: true
      - {from: exec, command: [pass, show, deploy]}
resources:
  - id: x
    type: file
";

#[test]
fn a_secret_can_name_further_places_to_look_in_order() {
    let text = resource_set(WITH_FALLBACK);
    valid(&text);
    let doc = document(&text);
    let secret = &doc.secrets()[0];
    assert!(secret.optional);
    assert_eq!(secret.fallback.len(), 3);
    let sources = secret.sources();
    assert_eq!(
        sources.iter().map(|s| s.from.as_str()).collect::<Vec<_>>(),
        ["secret-service", "keyring", "file", "exec"]
    );
    assert_eq!(sources[0], secret.source());
    assert_eq!(sources[1].key, "meridian:deploy");
    assert_eq!(sources[2].path, "/run/secrets/deploy");
    // A field of one place is that place's alone.
    assert!(sources[2].allow_wide_mode);
    assert!(!sources[0].allow_wide_mode && !sources[3].allow_wide_mode);
    assert_eq!(sources[3].command, ["pass", "show", "deploy"]);
    assert!(sources[1].attributes.is_empty());

    // An empty list is no further place.
    let none = resource_set(
        "secrets:\n  - {name: a, from: env, key: K, fallback: []}\nresources:\n  - id: x\n    \
         type: file\n",
    );
    valid(&none);
    assert_eq!(document(&none).secrets()[0].sources().len(), 1);
}

#[test]
fn a_further_place_is_checked_as_the_first_is_and_named_by_where_it_is() {
    for (entry, parts) in [
        (
            "{name: a, from: env, key: K, fallback: [{key: K2}]}",
            vec![
                "spec.secrets[0].fallback[0].from",
                "a secret needs a provider",
            ],
        ),
        (
            "{name: a, from: env, key: K, fallback: [{from: env, key: K2}, {from: vault}]}",
            vec![
                "spec.secrets[0].fallback[1].from",
                "\"vault\" is not a provider this version has",
            ],
        ),
        (
            "{name: a, from: env, key: K, fallback: [{from: keyring}]}",
            vec![
                "spec.secrets[0].fallback[0].key",
                "from: keyring needs key, the description of the key in the kernel keyring",
            ],
        ),
        (
            "{name: a, from: env, key: K, fallback: [{from: keychain, service: s}]}",
            vec![
                "spec.secrets[0].fallback[0].account",
                "from: keychain needs account",
            ],
        ),
        (
            "{name: a, from: env, key: K, fallback: [{from: secret-service}]}",
            vec![
                "spec.secrets[0].fallback[0].attributes",
                "from: secret-service needs attributes",
            ],
        ),
        (
            "{name: a, from: env, key: K, fallback: [{from: file, path: /x, key: K}]}",
            vec![
                "spec.secrets[0].fallback[0].key",
                "key means nothing to from: file",
            ],
        ),
        (
            "{name: a, from: keyring, key: K, attributes: {app: m}}",
            vec![
                "spec.secrets[0].attributes",
                "attributes means nothing to from: keyring",
            ],
        ),
        (
            "{name: a, from: systemd-creds, key: K, allowWideMode: true}",
            vec![
                "spec.secrets[0].allowWideMode",
                "allowWideMode means nothing to from: systemd-creds",
            ],
        ),
    ] {
        refused(
            &resource_set(&format!(
                "secrets:\n  - {entry}\nresources:\n  - id: x\n    type: file\n"
            )),
            &parts,
        );
    }
}

#[test]
fn a_further_place_takes_only_what_says_where_and_never_a_value() {
    let read = |entry: &str| {
        parse(
            "doc.yaml",
            resource_set(&format!(
                "secrets:\n  - {entry}\nresources:\n  - id: x\n    type: file\n"
            ))
            .as_bytes(),
        )
        .unwrap_err()
        .to_string()
    };
    // A further place is a place: it has no name of its own, is not
    // optional on its own, and has no places of its own.
    for field in ["name: b", "optional: true", "fallback: []"] {
        let err = read(&format!(
            "{{name: a, from: env, key: K, fallback: [{{from: env, key: K2, {field}}}]}}"
        ));
        let name = field.split(':').next().unwrap();
        assert!(
            err.contains("spec.secrets[0].fallback[0]") && err.contains(name),
            "{err}"
        );
    }
    // There is nowhere to write a value, and what was written is not
    // repeated.
    let err =
        read("{name: a, from: env, key: K, fallback: [{from: env, key: K2, value: hunter2}]}");
    assert!(
        err.contains("spec.secrets[0].fallback[0]") && err.contains("value"),
        "{err}"
    );
    assert!(!err.contains("hunter2"), "{err}");
    // The list is a list of places, and attributes are text by name.
    let err = read("{name: a, from: env, key: K, fallback: {from: env, key: K2}}");
    assert!(
        err.contains("spec.secrets[0].fallback") && err.contains("expected a list"),
        "{err}"
    );
    let err = read("{name: a, from: secret-service, attributes: [app, meridian]}");
    assert!(err.contains("spec.secrets[0].attributes"), "{err}");
}

#[test]
fn a_further_place_cannot_be_interpolated_either() {
    let open = concat!("$", "{{");
    for (field, entry) in [
        (
            "service",
            format!("{{name: a, from: keychain, service: \"{open} data.s }}}}\", account: m}}"),
        ),
        (
            "account",
            format!("{{name: a, from: keychain, service: s, account: \"{open} data.a }}}}\"}}"),
        ),
        (
            "attributes",
            format!(
                "{{name: a, from: secret-service, attributes: {{app: \"{open} data.a }}}}\"}}}}"
            ),
        ),
        (
            "fallback[0].key",
            format!(
                "{{name: a, from: env, key: K, fallback: [{{from: keyring, key: \"{open} data.k \
                 }}}}\"}}]}}"
            ),
        ),
        (
            "fallback[1].path",
            format!(
                "{{name: a, from: env, key: K, fallback: [{{from: env, key: K2}}, {{from: file, \
                 path: \"/run/{open} data.env }}}}\"}}]}}"
            ),
        ),
    ] {
        refused(
            &resource_set(&format!(
                "secrets:\n  - {entry}\nresources:\n  - id: x\n    type: file\n"
            )),
            &[
                &format!("spec.secrets[0].{field}"),
                "where a secret comes from is written out",
            ],
        );
    }
}

// ---- reading -----------------------------------------------------------

#[test]
fn a_secret_that_is_read_must_be_declared() {
    let open = concat!("$", "{{");
    refused(
        &document_declaring(&format!(
            "resources:\n  - id: conf\n    type: file\n    params:\n      content: \"{open} \
             secret.db_pasword }}}}\"\n"
        )),
        &[
            "spec.resources[0].params.content",
            "secret.db_pasword is not declared",
            "spec.secrets declares: db_password, tls_key, token",
        ],
    );
    refused(
        &resource_set("resources:\n  - id: conf\n    type: file\n    when: secret.x == 'a'\n"),
        &["secret.x is not declared", "spec.secrets declares none"],
    );
    refused(
        &document_declaring("resources:\n  - id: conf\n    type: file\n    when: secret == 'a'\n"),
        &["secret must be followed by the name of a secret, as in secret.name"],
    );
}

#[test]
fn a_secret_is_refused_wherever_a_value_is_shown_stored_or_shapes_the_document() {
    let open = concat!("$", "{{");
    let read = format!("\"{open} secret.db_password }}}}\"");
    let why = "a secret cannot be used here";
    // The target and its version.
    let doc = format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata: {{name: site}}\nspec:\n  target: \
         {read}\n  targetHost: {{from: inline, host: h}}\n  secrets:\n    - {{name: db_password, \
         from: env, key: K}}\n  resources:\n    - id: x\n      type: file\n"
    );
    refused(&doc, &["spec.target", why]);
    refused(
        &document_declaring(&format!(
            "targetVersion: {read}\nresources:\n  - id: x\n    type: file\n"
        )),
        &["spec.targetVersion", why],
    );
    // A label.
    let doc = format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: site\n  labels:\n    \
         owner: {read}\nspec:\n  target: ansible\n  targetHost: {{from: inline, host: h}}\n  \
         secrets:\n    - {{name: db_password, from: env, key: K}}\n  resources:\n    - id: x\n      type: file\n"
    );
    refused(&doc, &["metadata.labels.owner", why]);
    // The host.
    let doc = format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata: {{name: site}}\nspec:\n  target: \
         ansible\n  targetHost: {{from: inline, host: {read}}}\n  secrets:\n    - {{name: \
         db_password, from: env, key: K}}\n  resources:\n    - id: x\n      type: file\n"
    );
    refused(&doc, &["spec.targetHost.host", why]);
    // A resource's state.
    refused(
        &document_declaring(&format!(
            "resources:\n  - id: conf\n    type: file\n    state: {read}\n"
        )),
        &["spec.resources[0].state", why],
    );
    // An output.
    refused(
        &infrastructure(&format!(
            "{DECLARED}resources:\n  - id: x\n    type: file\noutputs:\n  - name: password\n    value: {read}\n"
        )),
        &["spec.outputs[0].value", why],
    );
    // An id is part of the skeleton, like any interpolation.
    refused(
        &document_declaring(&format!("resources:\n  - id: {read}\n    type: file\n")),
        &["spec.resources[0].id", "skeleton"],
    );
}

#[test]
fn a_secret_reaching_a_shown_field_through_a_variable_is_refused_when_resolved() {
    let open = concat!("$", "{{");
    let doc = document(&format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: site\n  labels:\n    \
         owner: \"{open} var.who }}}}\"\nspec:\n  target: ansible\n  targetHost: {{from: inline, \
         host: h}}\n  secrets:\n    - {{name: db_password, from: env, key: K}}\n  variables:\n    \
         who: \"{open} secret.db_password }}}}\"\n  resources:\n    - id: x\n      type: file\n"
    ));
    // A variable may hold a secret, so this is valid until it is placed.
    assert!(Bundle::new([doc.clone()]).validate().is_ok());
    for secrets in [Secrets::declared(&doc), fetched(&doc)] {
        let err = resolve_with(&doc, &Data::new(), &secrets)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("metadata.labels.owner") && err.contains("a secret cannot be used here"),
            "{err}"
        );
        assert!(!err.contains(PASSWORD), "{err}");
    }
}

// ---- resolving ---------------------------------------------------------

fn site() -> String {
    let open = concat!("$", "{{");
    document_declaring(&format!(
        "variables:\n  url: \"postgres://app:{open} urlEncode(secret.db_password) }}}}@db/app\"\n\
         conditions:\n  has_token: secret.token != null\nresources:\n  - id: conf\n    type: \
         file\n    params:\n      content: \"password={open} secret.db_password }}}}\\n\"\n      \
         url: \"{open} var.url }}}}\"\n      key: \"{open} secret.tls_key }}}}\"\n      nested:\n        \
         list: [\"{open} secret.db_password }}}}\", plain]\n      plain: text\n  - id: login\n    \
         type: file\n    when: cond.has_token\n    params:\n      token: \"{open} secret.token \
         }}}}\"\n  - id: strict\n    type: file\n    when: secret.db_password == 'p@ss/w0rd-9fXw'\n"
    ))
}

#[test]
fn a_resolved_document_holds_which_secret_is_meant_and_never_its_value() {
    let doc = document(&site());
    valid(&site());
    let secrets = fetched(&doc);
    let resolved = kept(resolve_with(&doc, &Data::new(), &secrets).unwrap());

    assert!(is(
        &secrets,
        param(&resolved, "conf", "content"),
        &format!("password={PASSWORD}\n")
    ));
    assert!(is(
        &secrets,
        param(&resolved, "conf", "url"),
        "postgres://app:p%40ss%2Fw0rd-9fXw@db/app"
    ));
    assert!(is(
        &secrets,
        param(&resolved, "conf", "key"),
        "-----KEY-----\n"
    ));
    assert_eq!(param(&resolved, "conf", "plain"), &Value::from("text"));

    // Nothing that can be made of the document shows the secret.
    let shown = format!("{resolved:?}");
    assert!(
        !shown.contains(PASSWORD) && !shown.contains("p%40ss"),
        "{shown}"
    );
    assert!(shown.contains("(sensitive)"), "{shown}");
    for r in resolved.resources() {
        let written = meridian_ir::canonical_map(&r.params);
        assert!(
            !written.contains(PASSWORD) && !written.contains("KEY"),
            "{written}"
        );
    }
    assert_eq!(
        param(&resolved, "conf", "nested").canonical(),
        "{\"list\":[\"(sensitive)\",\"plain\"]}"
    );
    // A resolved document declares nothing more to fetch.
    assert!(resolved.secrets().is_empty());
}

#[test]
fn a_condition_on_a_secret_decides_and_its_report_holds_only_what_was_written() {
    let doc = document(&site());
    let secrets = fetched(&doc);
    let Resolution::Kept { document, removed } =
        resolve_with(&doc, &Data::new(), &secrets).unwrap()
    else {
        panic!("kept");
    };
    let ids: Vec<&str> = document.resources().iter().map(|r| r.id.as_str()).collect();
    // The optional token was not found, so `login` is removed; the password
    // matched, so `strict` stays.
    assert_eq!(ids, ["conf", "strict"]);
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].subject, "resource \"login\"");
    assert_eq!(removed[0].condition, "cond.has_token");

    // With the other answer.
    let mut other = Secrets::declared(&doc);
    other.hold("db_password", secret("something else"));
    other.hold("tls_key", secret("k"));
    other.hold("token", secret("tok_123"));
    let Resolution::Kept { document, removed } = resolve_with(&doc, &Data::new(), &other).unwrap()
    else {
        panic!("kept");
    };
    let ids: Vec<&str> = document.resources().iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["conf", "login"]);
    // The report says which condition was false: the document's own words,
    // which hold the text it compared with and nothing of the secret.
    assert_eq!(
        removed[0].condition,
        "secret.db_password == 'p@ss/w0rd-9fXw'"
    );
    assert!(is(&other, param(&document, "login", "token"), "tok_123"));
}

#[test]
fn without_fetching_a_secret_in_a_param_is_named_and_a_condition_on_one_is_an_error() {
    let open = concat!("$", "{{");
    // No condition reads a secret: the document resolves, and each secret
    // is described by name.
    let text = document_declaring(&format!(
        "resources:\n  - id: conf\n    type: file\n    params:\n      content: \
         \"password={open} secret.db_password }}}}\\n\"\n      url: \"postgres://app:{open} \
         urlEncode(secret.db_password) }}}}@db/app\"\n"
    ));
    let doc = document(&text);
    let secrets = Secrets::declared(&doc);
    let resolved = kept(resolve_with(&doc, &Data::new(), &secrets).unwrap());
    let describe = |name: &str| {
        let Value::Secret(at) = param(&resolved, "conf", name) else {
            panic!("a secret");
        };
        secrets.table().describe(*at)
    };
    assert_eq!(describe("content"), "password=(secret db_password)\n");
    assert_eq!(
        describe("url"),
        "postgres://app:urlEncode((secret db_password))@db/app"
    );
    // `resolve` is the same thing.
    assert!(matches!(
        resolve(&doc, &Data::new()),
        Ok(Resolution::Kept { .. })
    ));

    // A condition must know the value, and nothing fetched it.
    let doc = document(&site());
    let err = resolve(&doc, &Data::new()).unwrap_err().to_string();
    assert!(
        err.contains("is not fetched here")
            && err.contains("a secret is fetched by plan and apply, never by validate or compile"),
        "{err}"
    );
}

#[test]
fn an_optional_secret_that_was_not_found_is_null() {
    let open = concat!("$", "{{");
    let text = document_declaring(&format!(
        "resources:\n  - id: conf\n    type: file\n    params:\n      token: \"{open} \
         secret.token ?? 'anonymous' }}}}\"\n      set: \"{open} secret.token != null }}}}\"\n"
    ));
    let doc = document(&text);
    let secrets = fetched(&doc);
    let resolved = kept(resolve_with(&doc, &Data::new(), &secrets).unwrap());
    assert_eq!(param(&resolved, "conf", "token"), &Value::from("anonymous"));
    assert_eq!(param(&resolved, "conf", "set"), &Value::Bool(false));

    // Placed inside text with no default, null has no text form.
    let text = document_declaring(&format!(
        "resources:\n  - id: conf\n    type: file\n    params:\n      line: \"token={open} \
         secret.token }}}}\"\n"
    ));
    let doc = document(&text);
    let err = resolve_with(&doc, &Data::new(), &fetched(&doc))
        .unwrap_err()
        .to_string();
    assert!(err.contains("the value is null"), "{err}");
}

#[test]
fn the_secrets_are_wiped_when_their_holder_is_dropped() {
    let doc = document(&site());
    let secrets = fetched(&doc);
    let table = std::sync::Arc::clone(secrets.table());
    let resolved = kept(resolve_with(&doc, &Data::new(), &secrets).unwrap());
    assert!(table.held() >= 2);
    let Value::Secret(at) = param(&resolved, "conf", "content").clone() else {
        panic!("a secret");
    };
    drop(secrets);
    // The document still exists, and names a secret that is gone.
    assert_eq!(table.held(), 0);
    assert!(table.with(at, |_| ()).is_err());
    assert_eq!(table.describe(at), "(sensitive)");
}

#[test]
fn a_document_without_secrets_is_untouched_by_all_of_this() {
    let open = concat!("$", "{{");
    let text = resource_set(&format!(
        "resources:\n  - id: conf\n    type: file\n    params:\n      content: \"env={open} \
         data.env }}}}\"\n"
    ));
    let doc = document(&text);
    assert!(doc.secrets().is_empty());
    let data: Data = [("env".to_owned(), Value::from("prod"))]
        .into_iter()
        .collect();
    let resolved = kept(resolve(&doc, &data).unwrap());
    assert_eq!(
        param(&resolved, "conf", "content"),
        &Value::from("env=prod")
    );
}
