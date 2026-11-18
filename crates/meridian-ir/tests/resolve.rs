//! The resolve stage: variables, named conditions, `when` on documents,
//! outputs and edges, and interpolation in every place it is allowed, with
//! every way each can be wrong.

use meridian_ir::{
    parse, resolve, resolve_for, Bundle, Data, Document, PlacedEscape, Removed, Resolution,
    Secrets, Value,
};

fn data_of(text: &str) -> Data {
    let Value::Map(map) = meridian_ir::parse_value("data.yaml", text.as_bytes()).unwrap() else {
        panic!("the data is a mapping");
    };
    map
}

fn data() -> Data {
    data_of(
        "env: prod\nproject: shop\nreplicas: 3\nteam: web\nports: [80, 443]\n\
         shell: \"${HOME}/bin\"\nsite: {name: shop, tls: true}\nnothing: null\n",
    )
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

/// Every diagnostic `validate` gives for a document, as text.
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
        assert!(found[0].contains(part), "wanted {part:?} in {:?}", found[0]);
    }
}

fn kept_with(text: &str, data: &Data) -> (Document, Vec<Removed>) {
    valid(text);
    match resolve(&document(text), data) {
        Ok(Resolution::Kept { document, removed }) => (*document, removed),
        other => panic!("{other:?}"),
    }
}

fn kept(text: &str) -> (Document, Vec<Removed>) {
    kept_with(text, &data())
}

fn failure(text: &str) -> String {
    match resolve(&document(text), &data()) {
        Err(e) => e.to_string(),
        other => panic!("{other:?}"),
    }
}

fn param<'d>(doc: &'d Document, id: &str, name: &str) -> &'d Value {
    doc.resources()
        .iter()
        .find(|r| r.id == id)
        .unwrap_or_else(|| panic!("no resource {id}"))
        .params
        .get(name)
        .unwrap_or_else(|| panic!("no param {name}"))
}

fn text(s: &str) -> Value {
    Value::String(s.to_owned())
}

fn removed(found: &[Removed]) -> Vec<(&str, &str)> {
    found
        .iter()
        .map(|r| (r.subject.as_str(), r.condition.as_str()))
        .collect()
}

const ONE: &str = "resources:\n  - {id: a, type: file, params: {path: /a}}\n";

// ---------------------------------------------------------------- variables

#[test]
fn a_variable_is_worked_out_once_and_read_anywhere() {
    let (doc, _) = kept(&resource_set(
        "variables:\n  name: \"${{ data.project }}-${{ data.env }}\"\n  root: \"/srv/${{ var.name }}\"\n  \
         count: ${{ data.replicas * 2 }}\n  plain: 7\n  ports: ${{ data.ports }}\n  \
         nested: {a: [\"x-${{ data.env }}\", 2], b: {c: \"${{ var.count }}\"}}\n\
         resources:\n  - id: a\n    type: file\n    params:\n      path: ${{ var.root }}/index.html\n      \
         count: ${{ var.count }}\n      plain: ${{ var.plain }}\n      ports: ${{ var.ports }}\n      \
         first: ${{ var.ports[0] }}\n      nested: ${{ var.nested }}\n      inner: ${{ var.nested.b.c }}\n",
    ));
    assert_eq!(param(&doc, "a", "path"), &text("/srv/shop-prod/index.html"));
    assert_eq!(param(&doc, "a", "count"), &Value::Int(6));
    assert_eq!(param(&doc, "a", "plain"), &Value::Int(7));
    assert_eq!(
        param(&doc, "a", "ports"),
        &Value::List(vec![Value::Int(80), Value::Int(443)])
    );
    assert_eq!(param(&doc, "a", "first"), &Value::Int(80));
    assert_eq!(param(&doc, "a", "inner"), &Value::Int(6));
    let Value::Map(nested) = param(&doc, "a", "nested") else {
        panic!("a mapping");
    };
    assert_eq!(
        nested["a"],
        Value::List(vec![text("x-prod"), Value::Int(2)])
    );
    // Nothing is left to decide in what a plugin is given.
    assert!(doc.variables().is_empty() && doc.conditions().is_empty());
}

#[test]
fn settings_are_placed_as_params_are() {
    let (doc, _) = kept(&resource_set(
        "variables:\n  transport: \"${{ data.env == 'prod' ? 'WinRM' : 'SSH' }}\"\n\
         settings:\n  target: \"${{ var.transport }}\"\n  engine: DscV2\n  depth: ${{ data.replicas }}\n\
         resources:\n  - {id: a, type: file, params: {path: /a}}\n",
    ));
    assert_eq!(doc.settings()["target"], text("WinRM"));
    assert_eq!(doc.settings()["engine"], text("DscV2"));
    assert_eq!(doc.settings()["depth"], Value::Int(3));
    // A settings key is a name the target reads, so it cannot be computed.
    refused(
        &resource_set(
            "settings:\n  \"${{ data.env }}\": x\nresources:\n  - {id: a, type: file, params: {path: /a}}\n",
        ),
        &["spec.settings.${{ data.env }}", "a setting's name"],
    );
}

#[test]
fn variables_may_be_written_in_any_order() {
    let (doc, _) = kept(&resource_set(
        "variables:\n  a: \"${{ var.b }}!\"\n  b: \"${{ var.c }}-${{ cond.z ? 'p' : 'd' }}\"\n  c: ${{ data.env }}\n\
         conditions:\n  z: var.c == 'prod'\n\
         resources:\n  - {id: a, type: file, params: {path: \"${{ var.a }}\"}}\n",
    ));
    assert_eq!(param(&doc, "a", "path"), &text("prod-p!"));
}

#[test]
fn a_variable_can_be_written_in_full() {
    let (doc, _) = kept(&resource_set(
        "variables:\n  root:\n    value: \"/srv/${{ data.project }}\"\n    description: where the site lives\n  \
         shape: {name: x, size: 2}\n\
         resources:\n  - {id: a, type: file, params: {path: \"${{ var.root }}\", size: \"${{ var.shape.size }}\"}}\n",
    ));
    assert_eq!(param(&doc, "a", "path"), &text("/srv/shop"));
    // A mapping without a `value` key is the value itself.
    assert_eq!(param(&doc, "a", "size"), &Value::Int(2));
}

#[test]
fn what_a_variable_cannot_be_is_refused_by_validate() {
    let with = |variables: &str| resource_set(&format!("variables:\n{variables}{ONE}"));
    refused(
        &with("  my-name: 1\n"),
        &["spec.variables.my-name", "not a usable name", "var.<name>"],
    );
    refused(
        &with("  1st: 1\n"),
        &["spec.variables.1st", "not a usable name"],
    );
    refused(
        &with("  a: ${{ var.missing }}\n"),
        &[
            "spec.variables.a",
            "var.missing is not declared",
            "spec.variables declares: a",
        ],
    );
    refused(
        &with("  a: ${{ cond.missing }}\n"),
        &[
            "cond.missing is not declared",
            "spec.conditions declares none",
        ],
    );
    refused(
        &with("  a: ${{ var }}\n"),
        &["var must be followed by the name of a variable"],
    );
    refused(
        &with("  a: ${{ param.x }}\n"),
        &["spec.variables.a", "unknown name \"param\""],
    );
    refused(
        &with("  a: \"${{ data.env\"\n"),
        &["spec.variables.a", "never closed"],
    );
    refused(
        &with("  a: {b: [\"ok\", \"${{ nope() }}\"]}\n"),
        &["spec.variables.a.b[1]", "unknown function \"nope\""],
    );
    refused(
        &with("  a: \"${web.public_ip}\"\n"),
        &[
            "spec.variables.a",
            "cannot hold a ${id.path} reference",
            "where it is used",
        ],
    );
    refused(
        &with("  a: {value: 1, type: number}\n"),
        &[
            "spec.variables.a",
            "\"type\" on a variable is not available yet",
        ],
    );
    refused(
        &with("  a: {value: 1, colour: red}\n"),
        &["spec.variables.a", "unknown field \"colour\""],
    );
    refused(
        &with("  a: {\"k-${{ data.env }}\": 1}\n"),
        &["a mapping key cannot be interpolated"],
    );
    // An escaped reference and an escaped interpolation are plain text.
    valid(&with("  a: \"$${HOME} and $${{ x }}\"\n"));
}

#[test]
fn variables_that_read_each_other_in_a_circle_are_refused_by_name() {
    refused(
        &resource_set(&format!(
            "variables:\n  a: ${{{{ var.b }}}}\n  b: ${{{{ var.c }}}}\n  c: ${{{{ var.a }}}}\n  d: 1\n{ONE}"
        )),
        &["spec.variables.a", "circle", "var.a -> var.b -> var.c -> var.a"],
    );
    refused(
        &resource_set(&format!("variables:\n  a: ${{{{ var.a }}}}\n{ONE}")),
        &["circle", "var.a -> var.a"],
    );
    // Through a named condition.
    refused(
        &resource_set(&format!(
            "variables:\n  a: \"${{{{ cond.x ? 1 : 2 }}}}\"\nconditions:\n  x: var.a == 1\n{ONE}"
        )),
        &["circle", "var.a -> cond.x -> var.a"],
    );
    // resolve refuses it as well, for a caller that did not validate.
    let err = failure(&resource_set(&format!(
        "variables:\n  a: ${{{{ var.b }}}}\n  b: ${{{{ var.a }}}}\n{ONE}"
    )));
    assert!(err.contains("var.a -> var.b -> var.a"), "{err}");
}

#[test]
fn a_variable_that_cannot_be_worked_out_is_an_error_and_never_empty() {
    let err = failure(&resource_set(&format!(
        "variables:\n  a: \"x-${{{{ data.missing }}}}\"\n{ONE}"
    )));
    assert!(
        err.contains("spec.variables.a") && err.contains("data.missing is not set"),
        "{err}"
    );
    let err = failure(&resource_set(&format!(
        "variables:\n  a: {{deep: [\"${{{{ data.env + 1 }}}}\"]}}\n{ONE}"
    )));
    assert!(err.contains("spec.variables.a.deep[0]"), "{err}");
}

// --------------------------------------------------------- named conditions

#[test]
fn a_named_condition_is_decided_once_and_read_as_cond() {
    let spec = "conditions:\n  is_prod: data.env == 'prod'\n  needs_tls: cond.is_prod || (data.force_tls ?? false)\n  \
                small: \"!cond.is_prod && data.replicas < 2\"\n  wrapped: ${{ data.replicas >= 3 }}\n\
                resources:\n  - {id: always, type: file, params: {path: /a}}\n  \
                - {id: cert, type: file, when: cond.needs_tls, params: {path: /c}}\n  \
                - {id: debug, type: file, when: \"!cond.is_prod\", params: {path: /d}}\n  \
                - {id: tiny, type: file, when: cond.small, params: {path: /t}}\n  \
                - {id: big, type: file, when: cond.wrapped && cond.is_prod, params: {path: /b, tls: \"${{ cond.needs_tls }}\"}}\n";
    let (doc, gone) = kept(&resource_set(spec));
    let ids: Vec<&str> = doc.resources().iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["always", "cert", "big"]);
    assert_eq!(param(&doc, "big", "tls"), &Value::Bool(true));
    assert_eq!(
        removed(&gone),
        [
            ("resource \"debug\"", "!cond.is_prod"),
            ("resource \"tiny\"", "cond.small"),
        ]
    );

    let (doc, _) = kept_with(
        &resource_set(spec),
        &data_of("env: dev\nreplicas: 1\nforce_tls: true\n"),
    );
    let ids: Vec<&str> = doc.resources().iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["always", "cert", "debug", "tiny"]);
}

#[test]
fn what_a_named_condition_cannot_be_is_refused_by_validate() {
    let with = |conditions: &str| resource_set(&format!("conditions:\n{conditions}{ONE}"));
    refused(
        &with("  is-prod: \"true\"\n"),
        &[
            "spec.conditions.is-prod",
            "not a usable name",
            "cond.<name>",
        ],
    );
    refused(
        &with("  a: \"\"\n"),
        &["spec.conditions.a", "needs an expression"],
    );
    refused(&with("  a: data.env ==\n"), &["spec.conditions.a"]);
    refused(
        &with("  a: cond.b\n"),
        &["cond.b is not declared", "spec.conditions declares: a"],
    );
    refused(
        &with("  a: facts.os.family == 'debian'\n"),
        &["facts are not available"],
    );
    refused(
        &with("  a: cond.b\n  b: \"!cond.a\"\n"),
        &["circle", "cond.a -> cond.b -> cond.a"],
    );
    refused(
        &with("  a: \"x ${{ data.env }}\"\n"),
        &["a condition is one expression, written bare"],
    );
}

#[test]
fn a_named_condition_that_is_not_a_yes_or_a_no_is_an_error() {
    let err = failure(&resource_set(&format!(
        "conditions:\n  a: data.replicas\n{ONE}"
    )));
    assert!(err.contains("spec.conditions.a"), "{err}");
    let err = failure(&resource_set(&format!(
        "conditions:\n  a: data.missing == 1\n{ONE}"
    )));
    assert!(
        err.contains("spec.conditions.a") && err.contains("data.missing is not set"),
        "{err}"
    );
}

// ------------------------------------------------------- when on a document

#[test]
fn spec_when_keeps_or_skips_the_whole_document() {
    let spec = format!("when: data.env == 'prod'\n{ONE}");
    let (doc, _) = kept(&resource_set(&spec));
    assert_eq!(doc.when(), "", "nothing is left to decide");
    valid(&resource_set(&spec));
    assert_eq!(
        resolve(&document(&resource_set(&spec)), &data_of("env: dev\n")),
        Ok(Resolution::Skipped {
            condition: "data.env == 'prod'".to_owned()
        })
    );
    // On both kinds, and through a named condition.
    let spec = format!("when: cond.on\nconditions: {{on: \"data.env != 'prod'\"}}\n{ONE}");
    assert!(matches!(
        resolve(&document(&infrastructure(&spec)), &data()),
        Ok(Resolution::Skipped { .. })
    ));
}

#[test]
fn a_skipped_document_does_not_decide_its_resources() {
    // What is inside a document that is not part of the run is not read: a
    // resource that needs data only its own environment has is not an error.
    let spec = "when: data.env == 'dev'\nresources:\n  - {id: a, type: file, when: data.only_in_dev, params: {path: \"${{ data.also_only_in_dev }}\"}}\n";
    assert!(matches!(
        resolve(&document(&resource_set(spec)), &data()),
        Ok(Resolution::Skipped { .. })
    ));
}

#[test]
fn a_spec_when_that_cannot_be_decided_is_an_error() {
    refused(
        &resource_set(&format!("when: data.env ==\n{ONE}")),
        &["spec.when"],
    );
    refused(
        &resource_set(&format!("when: cond.nope\n{ONE}")),
        &["spec.when", "cond.nope is not declared"],
    );
    let err = failure(&resource_set(&format!("when: data.missing\n{ONE}")));
    assert!(
        err.contains("spec.when") && err.contains("data.missing is not set"),
        "{err}"
    );
    let err = failure(&resource_set(&format!("when: data.env\n{ONE}")));
    assert!(err.contains("spec.when"), "{err}");
}

// ----------------------------------------------------------- when on an edge

const EDGES: &str = "resources:\n  - {id: pkg, type: package, params: {name: nginx}}\n  \
    - {id: conf, type: file, params: {path: /c}}\n  \
    - id: svc\n    type: service\n    params: {name: nginx}\n    dependsOn:\n      - {resource: pkg}\n      \
    - {resource: conf, when: data.env == 'prod'}\n    notifies:\n      \
    - {resource: conf, action: reload, when: \"data.env != 'prod'\"}\n      \
    - {resource: pkg, action: restart}\n";

#[test]
fn a_when_on_an_edge_drops_the_entry_and_leaves_both_resources() {
    let (doc, gone) = kept(&resource_set(EDGES));
    assert_eq!(doc.resources().len(), 3);
    let svc = &doc.resources()[2];
    let deps: Vec<&str> = svc.depends_on.iter().map(|d| d.resource.as_str()).collect();
    let notifies: Vec<&str> = svc.notifies.iter().map(|n| n.resource.as_str()).collect();
    assert_eq!(deps, ["pkg", "conf"]);
    assert_eq!(notifies, ["pkg"]);
    assert_eq!(
        removed(&gone),
        [("\"svc\" notifies \"conf\"", "data.env != 'prod'")]
    );
    assert!(
        svc.depends_on.iter().all(|d| d.when.is_empty())
            && svc.notifies.iter().all(|n| n.when.is_empty()),
        "nothing is left to decide"
    );

    let (doc, gone) = kept_with(&resource_set(EDGES), &data_of("env: dev\n"));
    let svc = &doc.resources()[2];
    let deps: Vec<&str> = svc.depends_on.iter().map(|d| d.resource.as_str()).collect();
    let notifies: Vec<&str> = svc.notifies.iter().map(|n| n.resource.as_str()).collect();
    assert_eq!(deps, ["pkg"]);
    assert_eq!(notifies, ["conf", "pkg"]);
    assert_eq!(
        removed(&gone),
        [("\"svc\" depends on \"conf\"", "data.env == 'prod'")]
    );
}

#[test]
fn an_edge_its_own_when_drops_does_not_need_if_missing() {
    // The entry is gone whatever became of the resource it named, so a
    // removed resource behind a dropped edge is not a dangling edge.
    let spec =
        "resources:\n  - {id: cert, type: file, when: data.site.tls, params: {path: /c}}\n  \
                - id: svc\n    type: service\n    params: {name: nginx}\n    dependsOn:\n      \
                - {resource: cert, when: data.site.tls}\n";
    let (doc, gone) = kept_with(&resource_set(spec), &data_of("site: {tls: false}\n"));
    assert_eq!(doc.resources().len(), 1);
    assert!(doc.resources()[0].depends_on.is_empty());
    assert_eq!(gone.len(), 2);

    // Without the edge's own condition, the old rule holds.
    let spec = spec.replace(", when: data.site.tls}\n", "}\n");
    let err = match resolve(
        &document(&resource_set(&spec)),
        &data_of("site: {tls: false}\n"),
    ) {
        Err(e) => e.to_string(),
        other => panic!("{other:?}"),
    };
    assert!(err.contains("ifMissing: skip"), "{err}");
}

#[test]
fn a_when_on_an_edge_is_checked_and_decided_like_any_other() {
    let edge = |when: &str| {
        resource_set(&format!(
            "resources:\n  - {{id: a, type: file, params: {{path: /a}}}}\n  - id: b\n    type: file\n    \
             params: {{path: /b}}\n    dependsOn:\n      - {{resource: a, when: \"{when}\"}}\n"
        ))
    };
    refused(
        &edge("data.env =="),
        &["spec.resources[1].dependsOn[0].when"],
    );
    refused(
        &edge("cond.nope"),
        &[
            "spec.resources[1].dependsOn[0].when",
            "cond.nope is not declared",
        ],
    );
    let err = failure(&edge("data.missing"));
    assert!(
        err.contains("spec.resources[1].dependsOn[0].when")
            && err.contains("resource \"b\" depends on \"a\"")
            && err.contains("data.missing is not set"),
        "{err}"
    );
}

// --------------------------------------------------------- when on an output

#[test]
fn a_when_on_an_output_declares_it_or_not() {
    let spec = "resources:\n  - {id: vm, type: aws_instance, params: {ami: x}}\noutputs:\n  \
                - {name: ip, value: \"${vm.public_ip}\"}\n  \
                - {name: dns, value: \"${vm.public_dns}\", when: data.env == 'prod'}\n  \
                - {name: debug, value: \"${vm.id}\", when: \"data.env != 'prod'\"}\n";
    let (doc, gone) = kept(&infrastructure(spec));
    let names: Vec<&str> = doc.outputs().iter().map(|o| o.name.as_str()).collect();
    assert_eq!(names, ["ip", "dns"]);
    assert_eq!(removed(&gone), [("output \"debug\"", "data.env != 'prod'")]);
    // A reference the tool resolves is left exactly as written.
    assert_eq!(doc.outputs()[0].value, "${vm.public_ip}");
    assert!(doc.outputs().iter().all(|o| o.when.is_empty()));

    refused(
        &infrastructure(&spec.replace("data.env == 'prod'", "data.env ==")),
        &["spec.outputs[1].when"],
    );
}

#[test]
fn an_output_value_can_be_interpolated() {
    let spec = "resources:\n  - {id: vm, type: aws_instance, params: {ami: x}}\noutputs:\n  \
                - {name: url, value: \"https://${vm.public_dns}/${{ data.project }}\"}\n  \
                - {name: env, value: \"${{ data.env }}\"}\n  - {name: n, value: \"${{ data.replicas }}\"}\n  \
                - {name: sh, value: \"${{ data.shell }}\"}\n";
    let (doc, _) = kept(&infrastructure(spec));
    let values: Vec<&str> = doc.outputs().iter().map(|o| o.value.as_str()).collect();
    assert_eq!(
        values,
        ["https://${vm.public_dns}/shop", "prod", "3", "$${HOME}/bin"]
    );
    let err = failure(&infrastructure(
        &spec.replace("data.replicas", "data.ports"),
    ));
    assert!(
        err.contains("spec.outputs[2].value") && err.contains("this field is text"),
        "{err}"
    );
}

// ------------------------------------------------------------ interpolation

#[test]
fn every_value_inside_params_is_interpolated_however_deep() {
    let (doc, _) = kept(&infrastructure(
        "resources:\n  - id: web\n    type: aws_instance\n    params:\n      \
         instance_type: \"${{ data.env == 'prod' ? 'm5.large' : 't3.micro' }}\"\n      \
         count: ${{ data.replicas ?? 1 }}\n      enabled: ${{ data.site.tls }}\n      \
         nothing: ${{ data.nothing }}\n      untouched: 5\n      \
         tags:\n        Name: ${{ data.project }}-${{ data.env }}\n        Owner: ${{ upper(data.team) }}\n      \
         ingress:\n        - {port: \"${{ data.ports[0] }}\", cidr: \"${{ cidrSubnet('10.0.0.0/16', 8, 1) }}\"}\n        \
         - [\"${{ data.ports[1] }}\", [\"deep-${{ data.env }}\"]]\n",
    ));
    assert_eq!(param(&doc, "web", "instance_type"), &text("m5.large"));
    assert_eq!(param(&doc, "web", "count"), &Value::Int(3));
    assert_eq!(param(&doc, "web", "enabled"), &Value::Bool(true));
    assert_eq!(param(&doc, "web", "nothing"), &Value::Null);
    assert_eq!(param(&doc, "web", "untouched"), &Value::Int(5));
    let Value::Map(tags) = param(&doc, "web", "tags") else {
        panic!("a mapping")
    };
    assert_eq!(tags["Name"], text("shop-prod"));
    assert_eq!(tags["Owner"], text("WEB"));
    let Value::List(ingress) = param(&doc, "web", "ingress") else {
        panic!("a list")
    };
    let Value::Map(first) = &ingress[0] else {
        panic!("a mapping")
    };
    assert_eq!(first["port"], Value::Int(80));
    assert_eq!(first["cidr"], text("10.0.1.0/24"));
    assert_eq!(
        ingress[1],
        Value::List(vec![Value::Int(443), Value::List(vec![text("deep-prod")])])
    );
}

#[test]
fn a_reference_the_tool_resolves_is_left_exactly_as_written() {
    let (doc, _) = kept(&infrastructure(
        "resources:\n  - {id: net, type: aws_vpc, params: {cidr: 10.0.0.0/16}}\n  \
         - id: web\n    type: aws_instance\n    params:\n      vpc: \"${net.id}\"\n      \
         both: \"${net.id}-${{ data.env }}\"\n      escaped: \"$${HOME} $${{ data.env }}\"\n      \
         tool: \"{{ ansible_hostname }} %{ if x }y%{ endif }\"\n",
    ));
    assert_eq!(param(&doc, "web", "vpc"), &text("${net.id}"));
    assert_eq!(param(&doc, "web", "both"), &text("${net.id}-prod"));
    assert_eq!(
        param(&doc, "web", "escaped"),
        &text("$${HOME} $${{ data.env }}")
    );
    assert_eq!(
        param(&doc, "web", "tool"),
        &text("{{ ansible_hostname }} %{ if x }y%{ endif }")
    );
}

#[test]
fn a_value_data_supplies_can_never_become_a_reference() {
    // data.shell is "${HOME}/bin". Placed into a param, it is literal text
    // to every plugin: the escape for a literal reference is applied.
    let (doc, _) = kept(&resource_set(
        "variables:\n  cmd: \"run ${{ data.shell }}\"\nresources:\n  - id: a\n    type: exec\n    params:\n      \
         whole: ${{ data.shell }}\n      inside: \"cd ${{ data.shell }}\"\n      \
         through_var: ${{ var.cmd }}\n      listed: ${{ [data.shell] }}\n",
    ));
    assert_eq!(param(&doc, "a", "whole"), &text("$${HOME}/bin"));
    assert_eq!(param(&doc, "a", "inside"), &text("cd $${HOME}/bin"));
    assert_eq!(param(&doc, "a", "through_var"), &text("run $${HOME}/bin"));
    assert_eq!(
        param(&doc, "a", "listed"),
        &Value::List(vec![text("$${HOME}/bin")])
    );
}

#[test]
fn state_target_version_target_host_and_labels_are_interpolated() {
    let text_of = "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: site\n  labels:\n    \
                   env: ${{ data.env }}\n    tier: web\n    owner: team-${{ data.team }}\n    n: ${{ data.replicas }}\n\
                   spec:\n  target: \"${{ data.env == 'prod' ? 'ansible' : 'chef' }}\"\n  \
                   targetVersion: \"${{ data.replicas }}.1\"\n  \
                   targetHost: {from: inline, host: \"${{ data.project }}.example.com\"}\n  \
                   resources:\n    - id: a\n      type: package\n      \
                   state: \"${{ data.site.tls ? 'present' : 'absent' }}\"\n      params: {name: nginx}\n";
    let (doc, _) = kept(text_of);
    assert_eq!(doc.target(), "ansible");
    assert_eq!(doc.target_version(), "3.1");
    assert_eq!(doc.resources()[0].state, "present");
    assert_eq!(doc.labels()["env"], "prod");
    assert_eq!(doc.labels()["tier"], "web");
    assert_eq!(doc.labels()["owner"], "team-web");
    assert_eq!(doc.labels()["n"], "3");
    let host = &doc
        .as_resource_set()
        .unwrap()
        .spec
        .target_host
        .as_ref()
        .unwrap()
        .host;
    assert_eq!(host, "shop.example.com");

    assert_eq!(
        meridian_ir::resolved_labels(&document(text_of), &data()).unwrap()["env"],
        "prod"
    );
    let err = meridian_ir::resolved_labels(&document(text_of), &data_of("team: x\n"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("metadata.labels.env") && err.contains("data.env is not set"),
        "{err}"
    );
}

#[test]
fn a_text_field_refuses_a_value_that_is_not_text() {
    let err = failure(&resource_set(
        "resources:\n  - {id: a, type: package, state: \"${{ data.ports }}\", params: {name: x}}\n",
    ));
    assert!(
        err.contains("spec.resources[0].state")
            && err.contains("this field is text")
            && err.contains("resource \"a\""),
        "{err}"
    );
    let err = failure(&resource_set(
        "resources:\n  - {id: a, type: package, state: \"${{ data.nothing }}\", params: {name: x}}\n",
    ));
    assert!(err.contains("this field is text"), "{err}");
    let err = failure(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata: {name: site}\nspec:\n  target: \"${{ data.empty ?? '' }}\"\n  \
         targetHost: {from: inline, host: h}\n  resources:\n    - {id: a, type: file, params: {path: /a}}\n",
    );
    assert!(
        err.contains("spec.target") && err.contains("needs a target"),
        "{err}"
    );
}

#[test]
fn the_skeleton_of_a_document_cannot_be_interpolated() {
    let one = |resource: &str| resource_set(&format!("resources:\n  - {resource}\n"));
    refused(
        &one("{id: \"a-${{ data.env }}\", type: file, params: {path: /a}}"),
        &["spec.resources[0].id", "cannot be interpolated", "skeleton"],
    );
    refused(
        &one("{id: a, type: \"${{ data.kind }}\", params: {path: /a}}"),
        &["spec.resources[0].type", "cannot be interpolated"],
    );
    refused(
        &one("{id: a, type: file, params: {\"p-${{ data.env }}\": 1}}"),
        &[
            "spec.resources[0].params.p-${{ data.env }}",
            "a param name cannot be interpolated",
        ],
    );
    refused(
        &one("{id: a, type: file, params: {tags: {\"k-${{ data.env }}\": 1}}}"),
        &["a mapping key cannot be interpolated"],
    );
    let found = problems(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: \"site-${{ data.env }}\"\n  \
         labels: {\"l-${{ data.env }}\": x}\nspec:\n  target: ansible\n  \
         targetHost: {from: infrastructure, ref: \"${{ data.ref }}\", output: \"${{ data.out }}\"}\n  \
         resources:\n    - {id: a, type: file, params: {path: /a}}\n",
    );
    for part in [
        "metadata.name: cannot be interpolated",
        "a label name cannot be interpolated",
        "spec.targetHost.ref: cannot be interpolated",
        "spec.targetHost.output: cannot be interpolated",
    ] {
        assert!(found.iter().any(|f| f.contains(part)), "{part}: {found:#?}");
    }
    let found = problems(&resource_set(
        "resources:\n  - {id: a, type: file, params: {path: /a}}\n  - id: b\n    type: file\n    params: {path: /b}\n    \
         dependsOn: [{resource: \"${{ data.dep }}\"}]\n    notifies: [{resource: a, action: \"${{ data.act }}\"}]\n",
    ));
    for part in [
        "spec.resources[1].dependsOn[0].resource: cannot be interpolated",
        "spec.resources[1].notifies[0].action: cannot be interpolated",
    ] {
        assert!(found.iter().any(|f| f.contains(part)), "{part}: {found:#?}");
    }
    refused(
        &infrastructure(
            "resources:\n  - {id: a, type: x, params: {p: 1}}\noutputs:\n  - {name: \"o-${{ data.env }}\", value: \"${a.id}\"}\n",
        ),
        &["spec.outputs[0].name", "cannot be interpolated"],
    );
}

#[test]
fn every_interpolation_is_checked_by_validate_without_any_data() {
    let one = |param: &str| {
        resource_set(&format!(
            "resources:\n  - {{id: a, type: file, params: {{path: \"{param}\"}}}}\n"
        ))
    };
    valid(&one("${{ data.anything.at.all }}"));
    refused(
        &one("${{ data.env"),
        &["spec.resources[0].params.path", "never closed"],
    );
    refused(&one("x-${{ }}"), &["empty interpolation"]);
    refused(&one("${{ endpoint.db }}"), &["unknown name \"endpoint\""]);
    refused(&one("${{ var.nope }}"), &["var.nope is not declared"]);
    refused(&one("${{ upper() }}"), &["upper"]);
    refused(&one("${{ facts.os }}"), &["facts are not available"]);
    refused(
        &resource_set(
            "resources:\n  - {id: a, type: file, state: \"${{ nope }}\", params: {path: /a}}\n",
        ),
        &["spec.resources[0].state", "unknown name \"nope\""],
    );
    refused(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata: {name: site, labels: {a: \"${{ nope }}\"}}\nspec:\n  \
         target: ansible\n  targetHost: {from: inline, host: h}\n  resources:\n    - {id: a, type: file, params: {path: /a}}\n",
        &["metadata.labels.a", "unknown name \"nope\""],
    );
}

#[test]
fn an_interpolation_that_fails_names_the_resource_and_the_place() {
    let err = failure(&resource_set(
        "resources:\n  - {id: a, type: file, params: {path: /a}}\n  \
         - {id: web, type: file, params: {tags: {Name: [\"ok\", \"x-${{ data.missing }}\"]}}}\n",
    ));
    assert!(
        err.contains("spec.resources[1].params.tags.Name[1]")
            && err.contains("resource \"web\"")
            && err.contains("in ${{ data.missing }}")
            && err.contains("data.missing is not set"),
        "{err}"
    );
}

#[test]
fn a_removed_resource_is_not_interpolated() {
    // What a condition removed is not part of the run, so what it would
    // have needed is not an error.
    let (doc, gone) = kept(&resource_set(
        "resources:\n  - {id: a, type: file, params: {path: /a}}\n  \
         - {id: dev_only, type: file, when: \"data.env == 'dev'\", params: {path: \"${{ data.only_in_dev }}\"}}\n",
    ));
    assert_eq!(doc.resources().len(), 1);
    assert_eq!(gone.len(), 1);
}

#[test]
fn a_condition_may_be_written_inside_braces_and_means_the_same() {
    let bare = resource_set(
        "resources:\n  - {id: a, type: file, params: {path: /a}}\n  - {id: b, type: file, when: \"data.env == 'prod'\", params: {path: /b}}\n",
    );
    let wrapped = bare.replace("\"data.env == 'prod'\"", "\"${{ data.env == 'prod' }}\"");
    assert_ne!(bare, wrapped);
    let ids = |text: &str, data: &Data| -> Vec<String> {
        kept_with(text, data)
            .0
            .resources()
            .iter()
            .map(|r| r.id.clone())
            .collect()
    };
    for data in [data(), data_of("env: dev\n")] {
        assert_eq!(ids(&bare, &data), ids(&wrapped, &data));
    }
}

#[test]
fn a_document_without_any_of_this_resolves_to_itself() {
    // Everything written before this stage existed must come out unchanged.
    let text = resource_set(
        "resources:\n  - {id: pkg, type: package, state: present, params: {name: nginx, opts: [\"${pkg.name}\", \"$${HOME}\", \"{{ x }}\"]}}\n  \
         - id: svc\n    type: service\n    params: {name: nginx}\n    dependsOn: [{resource: pkg}]\n    \
         notifies: [{resource: pkg, action: restart}]\n",
    );
    let doc = document(&text);
    let (resolved, gone) = kept(&text);
    assert_eq!(resolved, doc);
    assert!(gone.is_empty());
}

#[test]
fn resolving_is_the_same_for_every_target_and_both_kinds() {
    // The target takes no part in resolving: the same spec under any target
    // name keeps the same resources with the same params.
    let spec = "variables: {name: \"${{ data.project }}-${{ data.env }}\"}\nconditions: {prod: \"data.env == 'prod'\"}\n\
                resources:\n  - {id: a, type: t, params: {name: \"${{ var.name }}\", n: \"${{ data.replicas + 1 }}\"}}\n  \
                - {id: b, type: t, when: cond.prod, params: {x: 1}}\n  - {id: c, type: t, when: \"!cond.prod\", params: {x: 1}}\n";
    let mut seen = Vec::new();
    for target in [
        "ansible",
        "arm",
        "bicep",
        "chef",
        "cloud-init",
        "cloudformation",
        "docker",
        "dsc",
        "helm",
        "kubernetes",
        "opentofu",
        "openvox",
        "packer",
        "podman",
        "pulumi",
        "puppet",
        "salt",
        "terraform",
    ] {
        for kind in ["ResourceSet", "Infrastructure"] {
            let host = if kind == "ResourceSet" {
                "  targetHost: {from: inline, host: h}\n"
            } else {
                ""
            };
            let body: String = spec.lines().map(|l| format!("  {l}\n")).collect();
            let text = format!(
                "apiVersion: meridian/v1\nkind: {kind}\nmetadata: {{name: d}}\nspec:\n  target: {target}\n{host}{body}"
            );
            let (doc, gone) = kept(&text);
            let shape: Vec<(String, String)> = doc
                .resources()
                .iter()
                .map(|r| (r.id.clone(), Value::Map(r.params.clone()).canonical()))
                .collect();
            seen.push((shape, gone));
        }
    }
    assert_eq!(seen.len(), 36);
    assert!(seen.iter().all(|s| s == &seen[0]), "{seen:#?}");
    assert_eq!(seen[0].0.len(), 2);
}

#[test]
fn a_host_an_interpolation_places_is_held_to_what_a_name_can_be() {
    // Issue 134: validate cannot check a host that is decided from data, so
    // resolve does, once it is placed, and before any tool is pointed at it.
    let doc = "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata: {name: site}\nspec:\n  target: \
               ansible\n  targetHost: {from: inline, host: \"${{ data.host }}\"}\n  resources:\n    - \
               {id: a, type: file, params: {path: /a}}\n";
    for (host, says) in [
        ("-oProxyCommand=touch /tmp/pwned", "starts with \"-\""),
        ("web01 ansible_connection=local", "whitespace"),
        ("x' { } node 'y", "'\\''"),
    ] {
        let data = data_of(&format!("host: {host:?}\n"));
        let err = match resolve(&document(doc), &data) {
            Err(e) => e.to_string(),
            other => panic!("{host}: {other:?}"),
        };
        assert!(
            err.contains("spec.targetHost.host") && err.contains(says),
            "{host}: {err}"
        );
    }
    let (kept, _) = kept_with(doc, &data_of("host: deploy@web01.example.com\n"));
    let Document::ResourceSet(rs) = kept else {
        panic!("a ResourceSet");
    };
    assert_eq!(
        rs.spec.target_host.unwrap().host,
        "deploy@web01.example.com"
    );
}

#[test]
fn a_placed_value_is_made_literal_for_the_target_the_document_names() {
    // Issues 135 and 136: the openings of the tool's own expression syntax
    // are the plugin's to declare, by target name, and they apply to every
    // value placed after the target is decided, including an output's.
    let escapes_for = |target: &str| match target {
        "ansible" => vec![PlacedEscape::anywhere("{{", "{{ '{{' }}")],
        "arm" => vec![PlacedEscape::at_start("[", "[[")],
        _ => Vec::new(),
    };
    let data = data_of(
        "motd: \"hello {{ lookup('pipe','id') }}\"\nloc: \"[resourceGroup().id]\"\ntarget: \
         ansible\n",
    );
    let placed = |text: &str, id: &str, name: &str| {
        let doc = document(text);
        match resolve_for(&doc, &data, &Secrets::declared(&doc), &escapes_for).unwrap() {
            Resolution::Kept { document, .. } => param(&document, id, name).clone(),
            other => panic!("{other:?}"),
        }
    };

    let ansible = resource_set(
        "resources:\n  - {id: a, type: file, params: {path: /a, content: \"${{ data.motd }}\", \
         loc: \"${{ data.loc }}\"}}\n",
    );
    assert_eq!(
        placed(&ansible, "a", "content"),
        text("hello {{ '{{' }} lookup('pipe','id') }}")
    );
    // What ARM declares does not apply to Ansible.
    assert_eq!(placed(&ansible, "a", "loc"), text("[resourceGroup().id]"));

    // The target itself may be placed: the escapes follow what it becomes.
    let interpolated = ansible.replace("target: ansible", "target: \"${{ data.target }}\"");
    assert_eq!(
        placed(&interpolated, "a", "content"),
        text("hello {{ '{{' }} lookup('pipe','id') }}")
    );

    let arm = infrastructure(
        "resources:\n  - {id: a, type: x, params: {loc: \"${{ data.loc }}\", motd: \"${{ data.motd \
         }}\"}}\noutputs:\n  - {name: loc, value: \"${{ data.loc }}\"}\n",
    )
    .replace("target: terraform", "target: arm");
    assert_eq!(placed(&arm, "a", "loc"), text("[[resourceGroup().id]"));
    assert_eq!(
        placed(&arm, "a", "motd"),
        text("hello {{ lookup('pipe','id') }}")
    );
    let doc = document(&arm);
    let Resolution::Kept { document, .. } =
        resolve_for(&doc, &data, &Secrets::declared(&doc), &escapes_for).unwrap()
    else {
        panic!("kept");
    };
    let Document::Infrastructure(infra) = *document else {
        panic!("an Infrastructure");
    };
    assert_eq!(infra.spec.outputs[0].value, "[[resourceGroup().id]");

    // Without a plugin's word, only Meridian's own escape applies.
    let (kept, _) = kept_with(&ansible, &data);
    assert_eq!(
        param(&kept, "a", "content"),
        &text("hello {{ lookup('pipe','id') }}")
    );
}
