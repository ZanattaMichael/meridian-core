//! The compile half, without a process boundary.

#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};

use meridian_plugin_testkit::{assert_golden, graph_from_file, graph_from_text, secret_at};
use meridian_sdk::{Capabilities, Category, CompileError, Data, Emitted, Emitter, ResourceGraph};
use meridian_target_podman::{Podman, META_PATH};

fn golden() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn emit(graph: &ResourceGraph) -> Result<Emitted, CompileError> {
    Podman.emit(graph, &Data::new())
}

/// An infrastructure document with the given `spec` body, compiled.
fn compile(spec: &str) -> Result<Emitted, CompileError> {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: shop\nspec:\n  target: podman\n{spec}"
    );
    emit(&graph_from_text("infra.yaml", &text, "podman"))
}

/// One unit file of a compiled document.
fn unit(spec: &str, file: &str) -> String {
    let emitted = compile(spec).unwrap();
    emitted
        .artifact
        .files
        .get(file)
        .unwrap_or_else(|| panic!("no {file} among {:?}", emitted.artifact.files.keys()))
        .clone()
}

fn refusal(spec: &str) -> CompileError {
    compile(spec).expect_err("the document should be refused")
}

const WEB: &str =
    "    - id: web\n      type: container\n      params:\n        Image: quay.io/x/web:1\n";

#[test]
fn the_plugin_says_what_it_is() {
    assert_eq!(Podman.name(), "podman");
    assert_eq!(Podman.category(), Category::Containerization);
    assert_eq!(
        Podman.capabilities(),
        Capabilities {
            native_notify: true,
            synthetic_notify: false,
            runtime_condition: false,
            secrets: true,
            credentials: false,
        }
    );
    assert_eq!(
        Podman.supported_resource_types(),
        [
            "build",
            "container",
            "image",
            "kube",
            "network",
            "pod",
            "volume"
        ]
    );
}

#[test]
fn the_examples_compile_to_their_golden_units() {
    for example in ["web-app", "pod", "edge-host"] {
        let graph = graph_from_file(&format!("examples/podman/{example}.yaml"), "podman");
        assert_golden(&golden(), example, &emit(&graph).unwrap());
    }
}

#[test]
fn compiling_is_deterministic() {
    let graph = graph_from_file("examples/podman/web-app.yaml", "podman");
    assert_eq!(emit(&graph).unwrap(), emit(&graph).unwrap());
}

#[test]
fn each_type_becomes_its_own_kind_of_unit_and_service() {
    let emitted = compile(
        "  resources:\n    - id: a\n      type: container\n      params: {Image: quay.io/x/a:1}\n    - id: b\n      type: pod\n    - id: c\n      type: kube\n      params: {Yaml: /srv/c.yaml}\n    - id: d\n      type: volume\n    - id: e\n      type: network\n    - id: f\n      type: image\n      params: {Image: quay.io/x/f:1}\n    - id: g\n      type: build\n      params: {ImageTag: localhost/g, File: /srv/g/Containerfile}\n",
    )
    .unwrap();
    let files: Vec<&str> = emitted.artifact.files.keys().map(String::as_str).collect();
    assert_eq!(
        files,
        [
            "a.container",
            "b.pod",
            "c.kube",
            "d.volume",
            "e.network",
            "f.image",
            "g.build",
            META_PATH,
        ]
    );
    let meta = &emitted.artifact.files[META_PATH];
    for service in [
        "a.service",
        "b-pod.service",
        "c.service",
        "d-volume.service",
        "e-network.service",
        "f-image.service",
        "g-build.service",
    ] {
        assert!(
            meta.contains(&format!("\"service\": \"{service}\"")),
            "{service} in {meta}"
        );
    }
    // A unit with no settings still has its section.
    assert!(emitted.artifact.files["d.volume"].ends_with("\n[Volume]\n"));
}

#[test]
fn a_reference_becomes_the_unit_file_and_orders_without_being_told() {
    let text = unit(
        "  resources:\n    - id: data\n      type: volume\n    - id: db\n      type: container\n      params:\n        Image: quay.io/x/db:1\n        Volume: ${data}:/var/lib/db\n      dependsOn:\n        - resource: data\n",
        "db.container",
    );
    assert!(text.contains("Volume=data.volume:/var/lib/db\n"), "{text}");
    // Quadlet orders a unit after every unit file it names.
    assert!(!text.contains("[Unit]"), "{text}");
}

#[test]
fn an_order_the_keys_do_not_show_becomes_requires_and_after() {
    let text = unit(
        &format!(
            "  resources:\n    - id: db\n      type: container\n      params: {{Image: quay.io/x/db:1}}\n    - id: cache\n      type: volume\n{WEB}        unit:\n          After: network-online.target\n      dependsOn:\n        - resource: db\n        - resource: cache\n"
        ),
        "web.container",
    );
    assert!(
        text.contains(
            "[Unit]\nAfter=network-online.target\nAfter=cache-volume.service\nAfter=db.service\nRequires=cache-volume.service\nRequires=db.service\n"
        ),
        "{text}"
    );
}

#[test]
fn a_resource_is_known_by_its_file_its_name_and_its_service() {
    let text = unit(
        &format!(
            "  resources:\n    - id: db\n      type: container\n      params: {{Image: quay.io/x/db:1, ContainerName: shop-db}}\n    - id: net\n      type: network\n{WEB}        Network: ${{net}}\n        Environment:\n          DB_HOST: ${{db.name}}\n          NET: ${{net.name}}\n        unit:\n          BindsTo: ${{db.service}}\n"
        ),
        "web.container",
    );
    assert!(text.contains("Network=net.network\n"), "{text}");
    assert!(text.contains("Environment=DB_HOST=shop-db\n"), "{text}");
    assert!(text.contains("Environment=NET=systemd-net\n"), "{text}");
    assert!(text.contains("BindsTo=db.service\n"), "{text}");
}

#[test]
fn a_service_name_of_the_documents_own_is_the_one_used() {
    let emitted = compile(
        &format!(
            "  resources:\n    - id: db\n      type: volume\n      params: {{ServiceName: shop-storage}}\n{WEB}      dependsOn:\n        - resource: db\n"
        ),
    )
    .unwrap();
    assert!(
        emitted.artifact.files["web.container"].contains("Requires=shop-storage.service\n"),
        "{}",
        emitted.artifact.files["web.container"]
    );
    assert!(emitted.artifact.files[META_PATH].contains("\"service\": \"shop-storage.service\""));
}

#[test]
fn values_are_written_one_per_line() {
    let text = unit(
        &format!(
            "{}{WEB}        PublishPort: [\"8080:80\", 8443]\n        ReadOnly: true\n        PidsLimit: 100\n        Label:\n          tier: web\n          note: two words\n",
            "  resources:\n"
        ),
        "web.container",
    );
    assert!(
        text.contains("PublishPort=8080:80\nPublishPort=8443\n"),
        "{text}"
    );
    assert!(text.contains("ReadOnly=true\n"), "{text}");
    assert!(text.contains("PidsLimit=100\n"), "{text}");
    // A map is one name=value per entry, quoted when systemd would split it.
    assert!(
        text.contains("Label=\"note=two words\"\nLabel=tier=web\n"),
        "{text}"
    );
}

#[test]
fn a_maps_values_are_literal_and_a_strings_are_systemds() {
    let text = unit(
        &format!(
            "{}{WEB}        Environment:\n          PASSWORD: \"50%off$now\"\n          QUOTED: 'say \"hi\"'\n        HostName: web-%H\n        Exec: sh -c 'echo $${{HOME}}'\n",
            "  resources:\n"
        ),
        "web.container",
    );
    assert!(
        text.contains("Environment=PASSWORD=50%%off$$now\n"),
        "{text}"
    );
    assert!(
        text.contains("Environment=\"QUOTED=say \\\"hi\\\"\"\n"),
        "{text}"
    );
    // Written as a string, a value is systemd's to expand.
    assert!(text.contains("HostName=web-%H\n"), "{text}");
    assert!(text.contains("Exec=sh -c 'echo ${HOME}'\n"), "{text}");
}

#[test]
fn something_that_runs_is_started_at_boot_unless_the_document_says_otherwise() {
    let spec = format!("  resources:\n    - id: data\n      type: volume\n{WEB}");
    assert!(unit(&spec, "web.container").ends_with("\n[Install]\nWantedBy=default.target\n"));
    assert!(!unit(&spec, "data.volume").contains("[Install]"));

    let own = unit(
        &format!("  resources:\n{WEB}        install:\n          WantedBy: [multi-user.target, graphical.target]\n"),
        "web.container",
    );
    assert!(
        own.ends_with("\n[Install]\nWantedBy=multi-user.target\nWantedBy=graphical.target\n"),
        "{own}"
    );

    let none = unit(
        &format!("  resources:\n{WEB}        install: {{}}\n"),
        "web.container",
    );
    assert!(!none.contains("[Install]"), "{none}");
}

#[test]
fn the_sections_are_written_in_the_order_a_person_reads_them() {
    let text = unit(
        &format!(
            "  resources:\n{WEB}        quadlet: {{DefaultDependencies: false}}\n        service: {{Restart: always}}\n        unit: {{Description: Web}}\n"
        ),
        "web.container",
    );
    assert_eq!(
        text,
        "# Written by Meridian from the document \"shop\". The next apply overwrites any change made here.\n\
         \n[Unit]\nDescription=Web\n\
         \n[Container]\nImage=quay.io/x/web:1\n\
         \n[Service]\nRestart=always\n\
         \n[Quadlet]\nDefaultDependencies=false\n\
         \n[Install]\nWantedBy=default.target\n"
    );
}

#[test]
fn a_notify_becomes_part_of() {
    let text = unit(
        &format!(
            "  resources:\n    - id: conf\n      type: volume\n      notifies:\n        - resource: web\n          action: restart\n{WEB}"
        ),
        "web.container",
    );
    assert!(
        text.contains("[Unit]\nPartOf=conf-volume.service\n"),
        "{text}"
    );

    let err = refusal(&format!(
        "  resources:\n    - id: conf\n      type: volume\n      notifies:\n        - resource: web\n          action: reload\n{WEB}"
    ));
    assert_eq!(err.rule, "unsupported-action");
    assert_eq!(err.resource, "conf");
    assert_eq!(
        err.msg,
        "notifies resource \"web\" with action \"reload\", which this target cannot express: \
         what systemd can do to a unit when another changes is restart it (supported: restart)"
    );
}

#[test]
fn outputs_are_compiled_as_far_as_a_compile_can_know_them() {
    let emitted = compile(&format!(
        "  resources:\n    - id: data\n      type: volume\n{WEB}  outputs:\n    - name: service\n      value: unit ${{web.service}} runs ${{web.name}}\n    - name: mount\n      value: ${{data.inspect.Mountpoint}}/app\n"
    ))
    .unwrap();
    let meta = &emitted.artifact.files[META_PATH];
    assert!(
        meta.contains("\"unit web.service runs systemd-web\""),
        "{meta}"
    );
    assert!(
        meta.contains("\"inspect\": \"volume\",\n          \"name\": \"systemd-data\",\n          \"path\": \"Mountpoint\""),
        "{meta}"
    );
    assert!(meta.contains("\"/app\""), "{meta}");
}

#[test]
fn only_an_output_can_read_what_podman_knows_after_the_fact() {
    let err = refusal(&format!(
        "  resources:\n    - id: data\n      type: volume\n{WEB}        Environment:\n          DIR: ${{data.inspect.Mountpoint}}\n"
    ));
    assert_eq!(err.rule, "invalid-reference");
    assert_eq!(
        err.msg,
        "param \"Environment\": ${data.inspect.Mountpoint} is read from Podman once the thing \
         exists, so only an output can carry it; a unit file is written before anything runs"
    );

    let err = refusal(&format!(
        "  resources:\n{WEB}  outputs:\n    - name: bad\n      value: ${{web.inspect.{{.Id}}}}\n"
    ));
    assert_eq!(err.rule, "invalid-reference");
    assert!(err.msg.contains("a path of field names"), "{}", err.msg);
}

#[test]
fn a_reference_to_nothing_is_refused_with_the_way_to_escape_it() {
    let err = refusal(&format!(
        "  resources:\n{WEB}        Volume: ${{dta}}:/data\n"
    ));
    assert_eq!(err.rule, "invalid-reference");
    assert_eq!(err.resource, "web");
    assert_eq!(
        err.msg,
        "param \"Volume\": ${dta} refers to \"dta\", which is not a resource in this document. \
         To hand ${dta} to systemd as written, escape it: $${dta}"
    );
    assert_eq!(err.pos.file, "infra.yaml");

    let err = refusal(&format!(
        "  resources:\n{WEB}        HostName: ${{web.address}}\n"
    ));
    assert_eq!(
        err.msg,
        "param \"HostName\": ${web.address} reads \"address\", which this target does not \
         know. A resource offers its unit file (${web}), ${web.name} and ${web.service}; an \
         output may also read ${web.inspect.<Field>}"
    );

    let err = refusal(
        "  resources:\n    - id: stack\n      type: kube\n      params: {Yaml: /srv/stack.yaml}\n  outputs:\n    - name: n\n      value: ${stack.name}\n",
    );
    assert_eq!(
        err.msg,
        "output \"n\": ${stack.name}: a kube unit has no single name; its pods are named by its YAML"
    );
}

#[test]
fn a_type_podman_does_not_run_is_refused() {
    let err = refusal(
        "  resources:\n    - id: nginx\n      type: package\n      params: {name: nginx}\n",
    );
    assert_eq!(err.rule, "unsupported-resource-type");
    assert_eq!(
        err.msg,
        "\"package\" is not something Podman runs. This target accepts: build, container, \
         image, kube, network, pod, volume"
    );
}

#[test]
fn a_key_in_the_wrong_capitalisation_is_refused_with_the_right_one() {
    let err = refusal(
        "  resources:\n    - id: web\n      type: container\n      params: {image: quay.io/x/web:1}\n",
    );
    assert_eq!(err.rule, "unknown-param");
    assert_eq!(
        err.msg,
        "param \"image\" is Quadlet's \"Image\" in another capitalisation; Quadlet reads keys \
         exactly, so write \"Image\""
    );

    let err = refusal(&format!("  resources:\n{WEB}        ports: [80]\n"));
    assert_eq!(err.rule, "unknown-param");
    assert_eq!(
        err.msg,
        "param \"ports\" is not a Quadlet key of a container unit, which all start with a \
         capital letter (Image, Volume), nor one of the other sections of the file: unit, \
         service, install, quadlet"
    );
}

#[test]
fn a_key_this_plugin_does_not_know_is_kept_and_warned_about() {
    let emitted = compile(&format!(
        "  resources:\n{WEB}        BrandNewKey: yes-please\n"
    ))
    .unwrap();
    assert!(emitted.artifact.files["web.container"].contains("BrandNewKey=yes-please\n"));
    assert_eq!(emitted.warnings.len(), 1);
    assert_eq!(
        emitted.warnings[0].to_string(),
        "podman: web: \"BrandNewKey\" is not a key this plugin knows for a container unit. It \
         is written as given; a Podman that does not know it either will refuse the unit when \
         it is planned"
    );
}

#[test]
fn values_a_unit_file_cannot_hold_are_refused() {
    let err = refusal(&format!(
        "  resources:\n{WEB}        Exec: \"line one\\nline two\"\n"
    ));
    assert_eq!(err.rule, "invalid-param");
    assert_eq!(
        err.msg,
        "param \"Exec\": a value in a unit file is one line, and this one has a line break"
    );

    let err = refusal(&format!("  resources:\n{WEB}        HostName: ~\n"));
    assert_eq!(
        err.msg,
        "param \"HostName\": it has no value; leave the key out to leave it unset"
    );

    let err = refusal(&format!(
        "  resources:\n{WEB}        Environment:\n          NESTED: {{a: 1}}\n"
    ));
    assert_eq!(
        err.msg,
        "param \"Environment\": NESTED: a map's values are written as name=value, so each must \
         be a single value, not a mapping"
    );

    let err = refusal(&format!("  resources:\n{WEB}        service: always\n"));
    assert_eq!(
        err.msg,
        "param \"service\": the [Service] section is written as a map of its keys, not a string"
    );
}

#[test]
fn what_a_unit_cannot_do_without_is_asked_for() {
    let err = refusal(
        "  resources:\n    - id: web\n      type: container\n      params: {PublishPort: 80}\n",
    );
    assert_eq!(err.rule, "missing-param");
    assert_eq!(
        err.msg,
        "resource type \"container\" requires param \"Image\" or \"Rootfs\" on this target"
    );

    let err = refusal(
        "  resources:\n    - id: app\n      type: build\n      params: {ImageTag: localhost/app}\n",
    );
    assert_eq!(
        err.msg,
        "resource type \"build\" requires param \"File\" or \"SetWorkingDirectory\" on this \
         target, to say where the Containerfile is"
    );
}

#[test]
fn an_id_that_cannot_name_a_unit_is_refused() {
    let err = refusal(
        "  resources:\n    - id: web.app\n      type: container\n      params: {Image: quay.io/x/web:1}\n",
    );
    assert_eq!(err.rule, "invalid-name");
    assert_eq!(err.resource, "web.app");
}

#[test]
fn two_units_cannot_run_as_one_service() {
    let err = refusal(
        "  resources:\n    - id: db-volume\n      type: container\n      params: {Image: quay.io/x/db:1}\n    - id: db\n      type: volume\n",
    );
    assert_eq!(err.rule, "duplicate-service");
    assert_eq!(
        err.msg,
        "db-volume.container and db.volume would both run as db-volume.service; rename one of \
         the resources, or give one a ServiceName"
    );
}

#[test]
fn absent_and_runtime_conditions_are_refused_with_what_to_do_instead() {
    let err = refusal(
        "  resources:\n    - id: old\n      type: container\n      state: absent\n      params: {Image: quay.io/x/old:1}\n",
    );
    assert_eq!(err.rule, "unsupported-state");
    assert!(
        err.msg.contains("remove the resource from the document"),
        "{}",
        err.msg
    );

    let err = refusal(
        "  resources:\n    - id: web\n      type: container\n      state: paused\n      params: {Image: quay.io/x/web:1}\n",
    );
    assert_eq!(
        err.msg,
        "resource type \"container\" does not accept state \"paused\" on this target \
         (accepted: present, running)"
    );
}

#[test]
fn a_resource_set_compiles_too_and_cannot_carry_a_runtime_condition() {
    let text = "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: edge\nspec:\n  target: podman\n  targetHost:\n    from: inline\n    host: edge01.example.com\n  resources:\n    - id: web\n      type: container\n      runtimeWhen: facts.os == 'linux'\n      params: {Image: quay.io/x/web:1}\n";
    let err = emit(&graph_from_text("edge.yaml", text, "podman")).unwrap_err();
    assert_eq!(err.rule, "unsupported-runtime-condition");
    assert!(err.msg.contains("ConditionPathExists"), "{}", err.msg);

    let graph = graph_from_text(
        "edge.yaml",
        &text.replace("      runtimeWhen: facts.os == 'linux'\n", ""),
        "podman",
    );
    assert_eq!(graph.host, "edge01.example.com");
    assert!(emit(&graph)
        .unwrap()
        .artifact
        .files
        .contains_key("web.container"));
}

/// The first fenced block of a language in the user guide.
fn guide_block(guide: &str, language: &str) -> String {
    let open = format!("```{language}\n");
    let start = guide.find(&open).expect("the guide has such a block") + open.len();
    let end = start + guide[start..].find("```").expect("the block is closed");
    guide[start..end].to_owned()
}

#[test]
fn the_guides_first_document_compiles_to_the_unit_the_guide_shows() {
    let guide = std::fs::read_to_string(
        meridian_plugin_testkit::repo_root().join("docs/targets/podman.md"),
    )
    .unwrap();
    let emitted = emit(&graph_from_text(
        "hello.yaml",
        &guide_block(&guide, "yaml"),
        "podman",
    ))
    .unwrap();
    assert!(emitted.warnings.is_empty());
    assert_eq!(
        emitted.artifact.files["web.container"],
        guide_block(&guide, "ini")
    );
}

/// The document with the given place made a secret called `db_password`.
fn with_secret(spec: &str, id: &str, path: &[&str]) -> ResourceGraph {
    let text = format!(
        "apiVersion: meridian/v1\nkind: Infrastructure\nmetadata:\n  name: shop\nspec:\n  target: podman\n{spec}"
    );
    let mut graph = graph_from_text("infra.yaml", &text, "podman");
    secret_at(&mut graph, id, path, "db_password");
    graph
}

#[test]
fn a_secret_in_a_containers_environment_becomes_a_podman_secret_the_container_reads() {
    let graph = with_secret(
        &format!("  resources:\n{WEB}        Environment:\n          MODE: fast\n          PASSWORD: placeholder\n        Secret: other,type=mount\n"),
        "web",
        &["Environment", "PASSWORD"],
    );
    let text = &emit(&graph).unwrap().artifact.files["web.container"];
    assert!(text.contains("Environment=MODE=fast\n"), "{text}");
    assert!(!text.contains("PASSWORD="), "{text}");
    assert!(
        text.contains("Secret=other,type=mount\nSecret=meridian_secret_db_password,type=env,target=PASSWORD\n"),
        "{text}"
    );
    assert!(!text.contains("(secret "), "{text}");
    assert_eq!(
        meridian_target_podman::podman_secret("db_password"),
        "meridian_secret_db_password"
    );
    assert_eq!(
        meridian_target_podman::secret_reference("db_password", "PASSWORD"),
        "meridian_secret_db_password,type=env,target=PASSWORD"
    );
    assert!(Podman.capabilities().secrets);
}

#[test]
fn an_environment_that_is_only_secrets_leaves_no_empty_key_behind() {
    let graph = with_secret(
        &format!("  resources:\n{WEB}        Environment:\n          PASSWORD: placeholder\n"),
        "web",
        &["Environment", "PASSWORD"],
    );
    let text = &emit(&graph).unwrap().artifact.files["web.container"];
    assert!(!text.contains("Environment="), "{text}");
    assert!(
        text.contains("Secret=meridian_secret_db_password,type=env,target=PASSWORD\n"),
        "{text}"
    );
}

#[test]
fn a_secret_anywhere_but_a_containers_environment_is_refused_with_where_it_goes() {
    let cases: [(&str, &str, &[&str], &str); 4] = [
        (WEB, "web", &["Image"], "put it in Environment as a map"),
        (
            "    - id: data\n      type: volume\n      params:\n        Label: x\n",
            "data",
            &["Label"],
            "only a container's Environment can take one",
        ),
        (
            &format!("{WEB}        Environment:\n          PASSWORD: {{a: b}}\n"),
            "web",
            &["Environment", "PASSWORD", "a"],
            "write the secret as the whole value",
        ),
        (
            &format!("{WEB}        unit:\n          Description: x\n"),
            "web",
            &["unit", "Description"],
            "put it in Environment as a map",
        ),
    ];
    for (resources, id, path, said) in cases {
        let err = emit(&with_secret(
            &format!("  resources:\n{resources}"),
            id,
            path,
        ))
        .unwrap_err();
        assert_eq!(err.rule, "unsupported-secret-place", "{path:?}: {err}");
        assert!(err.msg.contains(said), "{path:?}: {err}");
        assert!(!err.msg.contains("(secret "), "{err}");
    }
}
