//! A credential outside a run: what a declaration must be, that resolving
//! one fetches only its own secrets into protected memory, and that nothing
//! it prints or fails with holds a value.

#![cfg(unix)]

use std::collections::BTreeMap;

use meridian_secret::{
    resolve_credential, CredentialDeclaration, CredentialError, FetchError, FieldSource, Source,
    CREDENTIAL_TYPES,
};

const PASSWORD: &str = "kq7Zp2-not-a-real-password-9fXw";

fn text(value: &str) -> FieldSource {
    FieldSource::Text(value.to_owned())
}

fn printed(value: &str) -> FieldSource {
    FieldSource::Secret(Source {
        from: "exec".to_owned(),
        command: vec!["printf".to_owned(), "%s".to_owned(), value.to_owned()],
        ..Default::default()
    })
}

fn declared(name: &str, kind: &str, fields: &[(&str, FieldSource)]) -> CredentialDeclaration {
    CredentialDeclaration {
        name: name.to_owned(),
        kind: kind.to_owned(),
        fields: fields
            .iter()
            .map(|(field, value)| ((*field).to_owned(), value.clone()))
            .collect::<BTreeMap<_, _>>(),
    }
}

fn registry() -> CredentialDeclaration {
    declared(
        "corp",
        "registry",
        &[
            ("server", text("registry.example.com")),
            ("user", text("ci")),
            ("password", printed(PASSWORD)),
        ],
    )
}

#[test]
fn a_credential_is_resolved_with_its_secret_fields_in_protected_memory() {
    let credential = resolve_credential(&[registry()], "corp").unwrap();
    assert_eq!(credential.name, "corp");
    assert_eq!(credential.kind, "registry");
    assert_eq!(credential.text("server"), Some("registry.example.com"));
    assert_eq!(credential.text("user"), Some("ci"));
    let password = credential.secret("password").unwrap();
    assert!(password.expose(|bytes| bytes == PASSWORD.as_bytes()));
    // A field is one or the other, and one that is not there is neither.
    assert!(credential.text("password").is_none());
    assert!(credential.secret("user").is_none());
    assert!(credential.text("domain").is_none());
    assert_eq!(
        credential.fields().collect::<Vec<_>>(),
        ["password", "server", "user"]
    );
    // It prints as what it is called, never what it holds.
    let shown = format!("{credential:?} {password:?} {password}");
    assert_eq!(
        shown,
        "Credential(\"corp\", registry) (sensitive) (sensitive)"
    );
}

#[test]
fn every_type_resolves_with_the_fields_it_has() {
    let token = declared(
        "bot",
        "token",
        &[("token", printed("tok_Zx81")), ("scheme", text("Bearer"))],
    );
    let login = declared(
        "admin",
        "usernamePassword",
        &[
            ("user", text("Administrator")),
            ("domain", text("CORP")),
            ("password", printed(PASSWORD)),
        ],
    );
    let project = [registry(), token, login];
    let bot = resolve_credential(&project, "bot").unwrap();
    assert_eq!(bot.text("scheme"), Some("Bearer"));
    assert!(bot.secret("token").unwrap().expose(|b| b == b"tok_Zx81"));
    let admin = resolve_credential(&project, "admin").unwrap();
    assert_eq!(admin.text("domain"), Some("CORP"));
    assert!(admin.secret("password").is_some());
    assert_eq!(CREDENTIAL_TYPES, ["registry", "token", "usernamePassword"]);
}

#[test]
fn only_the_credential_asked_for_is_fetched() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("credential-only");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let marker = dir.join("the-other-was-fetched");
    let other = declared(
        "other",
        "token",
        &[(
            "token",
            FieldSource::Secret(Source {
                from: "exec".to_owned(),
                command: vec!["touch".to_owned(), marker.display().to_string()],
                ..Default::default()
            }),
        )],
    );
    resolve_credential(&[other, registry()], "corp").unwrap();
    assert!(!marker.exists());
}

#[test]
fn a_credential_that_is_not_declared_is_refused_with_the_names_that_are() {
    let Err(error) = resolve_credential(&[registry()], "crop") else {
        panic!("crop is not declared")
    };
    assert_eq!(error, CredentialError::Undeclared(vec!["corp".to_owned()]));
    assert_eq!(error.to_string(), "it is not declared (declared: corp)");
    let Err(error) = resolve_credential(&[], "corp") else {
        panic!("nothing is declared")
    };
    assert_eq!(error.to_string(), "it is not declared (no credential is)");
}

#[test]
fn a_secret_written_out_is_refused_and_not_repeated() {
    let literal = declared(
        "corp",
        "registry",
        &[("user", text("ci")), ("password", text(PASSWORD))],
    );
    let problems = literal.problems();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].0, "password");
    assert_eq!(
        problems[0].1,
        "a password is written in the file; say where it comes from instead, as in password: \
         {from: env, key: ...}"
    );
    let Err(error) = resolve_credential(&[literal], "corp") else {
        panic!("a literal password is refused")
    };
    assert!(!error.to_string().contains(PASSWORD));
    assert!(!format!("{error:?}").contains(PASSWORD));
}

#[test]
fn every_rule_of_a_declaration() {
    let problems = |c: CredentialDeclaration| {
        c.problems()
            .into_iter()
            .map(|(field, why)| format!("{field}: {why}"))
            .collect::<Vec<_>>()
    };
    assert!(problems(registry()).is_empty());
    assert_eq!(
        problems(declared("", "token", &[("token", printed("t"))])),
        ["name: a credential needs a name; it is used by that name"]
    );
    assert_eq!(
        problems(declared("a", "", &[])),
        ["type: a credential needs a type: one of registry, token, usernamePassword"]
    );
    assert_eq!(
        problems(declared("a", "ssh", &[("user", text("deploy"))])),
        [
            "type: \"ssh\" is not a credential type this version has; type is one of registry, \
          token, usernamePassword"
        ]
    );
    assert_eq!(
        problems(declared("a", "token", &[])),
        ["token: a token credential needs token"]
    );
    assert_eq!(
        problems(declared(
            "a",
            "token",
            &[("token", printed("t")), ("user", text("ci"))]
        )),
        ["user: user is not a field of a token credential; its fields are scheme, token"]
    );
    assert_eq!(
        problems(declared(
            "a",
            "registry",
            &[("password", printed("p")), ("user", printed("ci"))]
        )),
        ["user: user is not secret; write its value"]
    );
    // What is wrong with where a secret comes from is said under its field.
    let no_key = FieldSource::Secret(Source {
        from: "env".to_owned(),
        ..Default::default()
    });
    assert_eq!(
        problems(declared("a", "token", &[("token", no_key)])),
        ["token.key: from: env needs key, the environment variable to read"]
    );
}

#[test]
fn a_failed_fetch_names_the_field_and_the_provider_and_nothing_read() {
    let failing = declared(
        "corp",
        "registry",
        &[(
            "password",
            FieldSource::Secret(Source {
                from: "exec".to_owned(),
                command: vec![
                    "sh".to_owned(),
                    "-c".to_owned(),
                    format!("echo {PASSWORD}; echo {PASSWORD} >&2; exit 3"),
                ],
                ..Default::default()
            }),
        )],
    );
    let Err(error) = resolve_credential(&[failing], "corp") else {
        panic!("the command fails")
    };
    assert_eq!(
        error,
        CredentialError::Fetch(
            "password".to_owned(),
            "exec".to_owned(),
            FetchError::Failed("the command sh exited with status 3".to_owned())
        )
    );
    assert_eq!(
        error.to_string(),
        "password (from: exec): the command sh exited with status 3"
    );
    assert!(!format!("{error:?}").contains(PASSWORD));
}
