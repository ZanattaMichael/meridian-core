//! `spec.credentials`: how an identity is declared, that its secret fields
//! are secrets and nothing else, and how a host names one.

use meridian_ir::{
    parse, resolve_with, secret_reference, Bundle, Data, Document, Resolution, Secrets, Value,
    CREDENTIAL_FROM_AMBIENT, HOST_CREDENTIAL_TYPES,
};
use meridian_secret::SecretValue;

const KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\nnot-a-real-key-9fXw\n-----END\n";

fn secret(text: &str) -> SecretValue {
    SecretValue::from_reader(text.as_bytes(), 4096).unwrap()
}

/// A ResourceSet whose `spec` holds `spec`, indented by two spaces, with
/// a host `h` reached as `credential` when one is named.
fn resource_set(credential: &str, spec: &str) -> String {
    let body: String = spec.lines().map(|l| format!("  {l}\n")).collect();
    let credential = if credential.is_empty() {
        String::new()
    } else {
        format!(", credential: {credential}")
    };
    format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata: {{name: site}}\nspec:\n  target: \
         ansible\n  targetHost: {{from: inline, host: h{credential}}}\n{body}"
    )
}

fn infrastructure(spec: &str) -> String {
    let body: String = spec.lines().map(|l| format!("  {l}\n")).collect();
    format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata: {{name: net}}\nspec:\n  target: \
         terraform\n{body}"
    )
}

const SECRETS: &str = "secrets:
  - name: deploy_key
    from: env
    key: DEPLOY_KEY
  - name: deploy_password
    from: env
    key: DEPLOY_PASSWORD
";

const RESOURCES: &str = "resources:
  - id: x
    type: file
    params:
      path: /etc/motd
";

/// A document declaring the two secrets, the credentials in `credentials`
/// (each entry a YAML mapping on one line), one resource, and a host reached
/// as `host_credential`.
fn with(host_credential: &str, credentials: &[&str]) -> String {
    let entries: String = credentials.iter().map(|c| format!("  - {c}\n")).collect();
    resource_set(
        host_credential,
        &format!("{SECRETS}credentials:\n{entries}{RESOURCES}"),
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

fn fetched(doc: &Document) -> Secrets {
    let mut secrets = Secrets::declared(doc);
    secrets.hold("deploy_key", secret(KEY));
    secrets.hold("deploy_password", secret("hunter2-9fXw"));
    secrets
}

fn kept(resolution: Resolution) -> Document {
    match resolution {
        Resolution::Kept { document, .. } => *document,
        Resolution::Skipped { condition } => panic!("skipped by {condition}"),
    }
}

const SSH_KEY: &str =
    "{name: deploy, type: ssh, user: deploy, privateKey: \"${{ secret.deploy_key }}\"}";
const SSH_PASSWORD: &str =
    "{name: deploy, type: ssh, user: deploy, password: \"${{ secret.deploy_password }}\"}";

// ---- declaring ---------------------------------------------------------

#[test]
fn a_credential_is_declared_by_name_and_type_with_the_fields_of_its_type() {
    let text = with("deploy", &[SSH_KEY]);
    valid(&text);
    let doc = document(&text);
    let credentials = doc.credentials();
    assert_eq!(credentials.len(), 1);
    let deploy = &credentials[0];
    assert_eq!(
        (deploy.name.as_str(), deploy.r#type.as_str()),
        ("deploy", "ssh")
    );
    assert_eq!(deploy.kind().map(|k| k.name), Some("ssh"));
    assert!(!deploy.is_ambient());
    assert_eq!(deploy.text("user"), Some("deploy"));
    assert!(!deploy.flag("agent"));
    // Before resolving, the secret field is the reference as written, and
    // the summary is the name, type and user: nothing of the key.
    assert_eq!(deploy.text("privateKey"), Some("${{ secret.deploy_key }}"));
    assert!(deploy.secrets().is_empty());
    assert_eq!(deploy.summary(), "deploy (ssh, user deploy)");
    assert_eq!(
        doc.credential("deploy").map(|c| c.name.as_str()),
        Some("deploy")
    );
    assert!(doc.credential("other").is_none());
    assert_eq!(
        match &doc {
            Document::ResourceSet(rs) => rs.spec.target_host.as_ref().unwrap().credential.as_str(),
            _ => "",
        },
        "deploy"
    );
}

#[test]
fn every_type_is_accepted_when_complete_and_an_infrastructure_takes_them_too() {
    let entries = [
        "{name: a, type: aws, accessKeyId: AKIA, secretAccessKey: \"${{ secret.deploy_key }}\", region: eu-west-1}",
        "{name: b, type: aws, profile: ci}",
        "{name: c, type: azure, clientId: id, tenantId: t, clientSecret: \"${{ secret.deploy_password }}\"}",
        "{name: d, type: azure, managedIdentity: true}",
        "{name: e, type: certificate, certificate: /etc/ssl/client.pem, privateKey: \"${{ secret.deploy_key }}\"}",
        "{name: f, type: gcp, project: p, serviceAccountKey: \"${{ secret.deploy_key }}\"}",
        "{name: g, type: kubeconfig, server: \"https://k8s.example.com\", token: \"${{ secret.deploy_password }}\"}",
        "{name: h, type: registry, server: registry.example.com, user: ci, password: \"${{ secret.deploy_password }}\"}",
        "{name: i, type: ssh, user: deploy, agent: true}",
        "{name: j, type: token, token: \"${{ secret.deploy_password }}\", scheme: Bearer}",
        "{name: k, type: usernamePassword, user: admin, domain: CORP, password: \"${{ secret.deploy_password }}\"}",
    ];
    valid(&with("", &entries));
    let credentials: String = entries.iter().map(|c| format!("  - {c}\n")).collect();
    valid(&infrastructure(&format!(
        "{SECRETS}credentials:\n{credentials}{RESOURCES}"
    )));
}

#[test]
fn an_ambient_credential_needs_no_secret_and_is_the_tools_own() {
    let text = with(
        "deploy",
        &["{name: deploy, type: ssh, from: ambient, user: deploy}"],
    );
    valid(&text);
    let doc = document(&text);
    let deploy = doc.credential("deploy").unwrap();
    assert!(deploy.is_ambient());
    assert_eq!(deploy.from, CREDENTIAL_FROM_AMBIENT);
    assert_eq!(deploy.summary(), "deploy (ssh, user deploy, ambient)");
    valid(&with("", &["{name: cloud, type: aws, from: ambient}"]));
    // Any other `from` is refused, and the credential is not ambient, so it
    // is also incomplete.
    let found = problems(&with("", &["{name: cloud, type: aws, from: env}"]));
    assert_eq!(found.len(), 3, "{found:#?}");
    assert!(
        found[1].contains("spec.credentials[0].from")
            && found[1].contains("from is ambient, or left out")
            && found[1].contains("the tool finds the credential for itself"),
        "{}",
        found[1]
    );
    assert!(found[2].contains("an aws credential needs"), "{}", found[2]);
}

#[test]
fn a_declaration_that_is_incomplete_or_mixed_up_is_refused() {
    for (entry, parts) in [
        (
            "{type: ssh, agent: true}",
            vec!["spec.credentials[0].name", "a credential needs a name"],
        ),
        (
            "{name: 9lives, type: ssh, agent: true}",
            vec!["spec.credentials[0].name", "is not a usable name"],
        ),
        (
            "{name: has-dash, type: ssh, agent: true}",
            vec!["spec.credentials[0].name", "is not a usable name"],
        ),
        (
            "{name: a}",
            vec![
                "spec.credentials[0].type",
                "a credential needs a type: one of aws, azure, certificate, gcp, kubeconfig, \
                 registry, ssh, token, usernamePassword",
            ],
        ),
        (
            "{name: a, type: vault, token: \"${{ secret.deploy_password }}\"}",
            vec![
                "spec.credentials[0].type",
                "\"vault\" is not a credential type this version has",
                "type is one of aws, azure",
            ],
        ),
        (
            "{name: a, type: ssh, user: deploy}",
            vec![
                "spec.credentials[0]:",
                "an ssh credential needs privateKey, or password, or agent: true; or from: \
                 ambient, for one the tool finds itself",
            ],
        ),
        (
            "{name: a, type: aws, accessKeyId: AKIA}",
            vec![
                "spec.credentials[0]:",
                "an aws credential needs accessKeyId and secretAccessKey, or profile, or roleArn",
            ],
        ),
        (
            "{name: a, type: ssh, agent: true, region: eu-west-1}",
            vec![
                "spec.credentials[0].region",
                "region is not a field of an ssh credential; its fields are agent, certificate, \
                 passphrase, password, privateKey, user",
            ],
        ),
        (
            "{name: a, type: ssh, agent: yes}",
            vec!["spec.credentials[0].agent", "agent is true or false"],
        ),
        (
            "{name: a, type: ssh, agent: true, user: [deploy]}",
            vec!["spec.credentials[0].user", "user is text, found a list"],
        ),
    ] {
        refused(&with("", &[entry]), &parts);
    }
}

#[test]
fn a_field_no_type_has_is_refused_when_read_without_repeating_its_value() {
    let err = parse(
        "doc.yaml",
        with(
            "",
            &["{name: a, type: ssh, agent: true, port: 22, value: hunter2-literal}"],
        )
        .as_bytes(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("spec.credentials[0]") && err.contains("unknown field"),
        "{err}"
    );
    assert!(!err.contains("hunter2"), "{err}");
}

#[test]
fn a_credential_declared_twice_is_refused() {
    refused(
        &with("", &[SSH_KEY, "{name: deploy, type: ssh, agent: true}"]),
        &[
            "spec.credentials[1].name",
            "the credential \"deploy\" is declared twice",
        ],
    );
}

// ---- secret fields -----------------------------------------------------

#[test]
fn a_secret_field_is_one_declared_secret_and_nothing_else() {
    // A literal is refused, and the error does not repeat it.
    let found = problems(&with(
        "",
        &["{name: a, type: ssh, user: deploy, password: hunter2-literal}"],
    ));
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(
        found[0].contains("spec.credentials[0].password")
            && found[0].contains(
                "a password is written in the document; declare a secret and read it with \
                 ${{ secret.<name> }}"
            ),
        "{}",
        found[0]
    );
    assert!(!found[0].contains("hunter2"), "{}", found[0]);

    // So is anything but text.
    refused(
        &with("", &["{name: a, type: ssh, user: deploy, password: 12345}"]),
        &[
            "spec.credentials[0].password",
            "a password is written in the document",
        ],
    );
    // A secret the document does not declare.
    refused(
        &with(
            "",
            &["{name: a, type: ssh, user: deploy, password: \"${{ secret.root_password }}\"}"],
        ),
        &[
            "spec.credentials[0].password",
            "password reads secret.root_password, which spec.secrets does not declare \
             (declared: deploy_key, deploy_password)",
        ],
    );
    // A secret joined with text, or read from anywhere else.
    for value in [
        "\"${{ secret.deploy_password }}!\"",
        "\"${{ data.password }}\"",
        "\"${{ var.password }}\"",
        "\"pw-${{ secret.deploy_password }}\"",
    ] {
        refused(
            &with(
                "",
                &[&format!(
                    "{{name: a, type: ssh, user: deploy, password: {value}}}"
                )],
            ),
            &[
                "spec.credentials[0].password",
                "password is one declared secret, read as it is: ${{ secret.<name> }} and \
                 nothing more",
            ],
        );
    }
}

#[test]
fn a_plain_field_may_be_interpolated_but_never_holds_a_secret() {
    let open = concat!("$", "{{");
    // Text from data is fine in a plain field.
    valid(&with(
        "deploy",
        &[&format!(
            "{{name: deploy, type: ssh, user: \"{open} data.user }}}}\", agent: true}}"
        )],
    ));
    // A secret in a plain field is refused when the document is checked,
    // before anything is fetched.
    refused(
        &with(
            "deploy",
            &[&format!(
                "{{name: deploy, type: ssh, user: \"{open} secret.deploy_password }}}}\", agent: true}}"
            )],
        ),
        &["spec.credentials[0].user", "a secret cannot be used here"],
    );
}

#[test]
fn secret_reference_takes_exactly_one_secret_read_as_it_is() {
    assert_eq!(
        secret_reference("${{ secret.deploy_key }}"),
        Some("deploy_key")
    );
    assert_eq!(secret_reference("  ${{secret.k}}  "), Some("k"));
    assert_eq!(secret_reference("${{ secret.deploy_key }}!"), None);
    assert_eq!(secret_reference("${{ data.key }}"), None);
    assert_eq!(secret_reference("${{ secret.9k }}"), None);
    assert_eq!(secret_reference("${{ secret.a.b }}"), None);
    assert_eq!(secret_reference("plain"), None);
}

// ---- resolving ---------------------------------------------------------

#[test]
fn a_resolved_credential_holds_which_secret_each_field_is_and_never_its_value() {
    let doc = document(&with("deploy", &[SSH_KEY]));
    let secrets = fetched(&doc);
    let resolved = kept(resolve_with(&doc, &Data::new(), &secrets).unwrap());
    let deploy = resolved.credential("deploy").unwrap();
    assert_eq!(deploy.text("user"), Some("deploy"));
    assert_eq!(deploy.text("privateKey"), None);
    let held = deploy.secrets();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].0, "privateKey");
    let at = held[0].1;
    assert!(matches!(deploy.fields["privateKey"], Value::Secret(a) if a == at));
    assert!(secrets
        .table()
        .with(at, |value| value.expose(|bytes| bytes == KEY.as_bytes()))
        .unwrap());
    // Nothing printable about the document holds the key.
    let shown = format!("{resolved:?}\n{}", deploy.summary());
    assert!(!shown.contains("not-a-real-key"), "{shown}");
    assert_eq!(deploy.summary(), "deploy (ssh, user deploy)");
}

#[test]
fn a_plain_field_is_placed_from_data_and_an_ambient_credential_resolves_as_it_is() {
    let open = concat!("$", "{{");
    let doc = document(&with(
        "deploy",
        &[&format!(
            "{{name: deploy, type: ssh, user: \"{open} data.user }}}}\", from: ambient}}"
        )],
    ));
    let mut data = Data::new();
    data.insert("user".into(), Value::String("svc_deploy".into()));
    let resolved = kept(resolve_with(&doc, &data, &fetched(&doc)).unwrap());
    let deploy = resolved.credential("deploy").unwrap();
    assert_eq!(deploy.text("user"), Some("svc_deploy"));
    assert!(deploy.is_ambient() && deploy.secrets().is_empty());
}

#[test]
fn a_secret_that_was_not_found_fails_the_credential_and_names_the_field() {
    let doc = document(&with("deploy", &[SSH_PASSWORD]));
    let mut secrets = Secrets::declared(&doc);
    secrets.hold("deploy_key", secret(KEY));
    secrets.absent("deploy_password");
    let err = resolve_with(&doc, &Data::new(), &secrets)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("spec.credentials[0].password") && err.contains("password reads a secret"),
        "{err}"
    );
}

// ---- a host's credential -----------------------------------------------

#[test]
fn a_host_names_a_declared_ssh_or_username_password_credential() {
    assert_eq!(HOST_CREDENTIAL_TYPES, ["ssh", "usernamePassword"]);
    valid(&with("deploy", &[SSH_KEY]));
    valid(&with(
        "deploy",
        &["{name: deploy, type: usernamePassword, user: admin, password: \"${{ secret.deploy_password }}\"}"],
    ));
    refused(
        &with("deploy", &["{name: other, type: ssh, agent: true}"]),
        &[
            "spec.targetHost.credential",
            "no credential named \"deploy\" is declared in spec.credentials (declared: other)",
        ],
    );
    refused(
        &resource_set("deploy", RESOURCES),
        &[
            "spec.targetHost.credential",
            "no credential named \"deploy\" is declared in spec.credentials",
        ],
    );
    refused(
        &with("cloud", &["{name: cloud, type: aws, profile: ci}"]),
        &[
            "spec.targetHost.credential",
            "a host takes an ssh or usernamePassword credential; \"cloud\" is aws",
        ],
    );
    let open = concat!("$", "{{");
    refused(
        &with(&format!("\"{open} data.who }}}}\""), &[SSH_KEY]),
        &[
            "spec.targetHost.credential",
            "a credential is named as written",
            "it is not interpolated",
        ],
    );
}

#[test]
fn a_credential_a_host_does_not_name_is_still_checked_and_kept() {
    // Declared but unused: checked like any other, and resolved along with
    // the document so a later use finds it.
    let text = with("", &[SSH_KEY]);
    valid(&text);
    let doc = document(&text);
    let resolved = kept(resolve_with(&doc, &Data::new(), &fetched(&doc)).unwrap());
    assert_eq!(resolved.credentials().len(), 1);
    assert_eq!(resolved.credential("deploy").unwrap().secrets().len(), 1);
}
