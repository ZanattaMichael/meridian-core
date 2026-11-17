//! The running half, against stand-ins for `helm`, `kustomize` and
//! `kubectl`.

use std::fs;
use std::path::Path;

use meridian_plugin_testkit::{artifact, scratch, stand_in, KeptLog};
use meridian_sdk::{Artifact, RunContext, Runner, Value};
use meridian_target_helm::HelmRunner;

const TMP: &str = env!("CARGO_TARGET_TMPDIR");

fn release_json(kustomize: bool) -> String {
    let (prefix, namespace) = if kustomize {
        ("prod-", "prod")
    } else {
        ("", "")
    };
    format!(
        r#"{{"release":"web-app","namespace":"web","createNamespace":true,"wait":true,"atomic":false,"timeout":"5m",
"chart":{{"name":"web-app","version":"1.4.0"}},"kustomize":{kustomize},
"resources":{{
  "web":{{"apiVersion":"apps/v1","kind":"Deployment","name":"{prefix}web","namespace":"{namespace}"}},
  "web_config":{{"apiVersion":"v1","kind":"ConfigMap","name":"{prefix}web-config","namespace":"{namespace}"}},
  "web_svc":{{"apiVersion":"v1","kind":"Service","name":"{prefix}web","namespace":"{namespace}"}},
  "keys":{{"apiVersion":"v1","kind":"Secret","name":"{prefix}keys","namespace":"{namespace}"}}}},
"outputs":[
  {{"name":"address","sensitive":false,"parts":[{{"resource":"web_svc","path":["status","loadBalancer","ingress",0,"ip"]}}]}},
  {{"name":"url","sensitive":false,"parts":[{{"text":"http://"}},{{"resource":"web_svc","path":["status","loadBalancer","ingress",0,"ip"]}},{{"text":":"}},{{"resource":"web_svc","path":["spec","ports",0,"port"]}},{{"text":"/"}}]}},
  {{"name":"replicas","sensitive":false,"parts":[{{"resource":"web","path":["status","readyReplicas"]}}]}},
  {{"name":"token","sensitive":true,"parts":[{{"resource":"keys","path":["data","token"]}}]}},
  {{"name":"name","sensitive":false,"parts":[{{"text":"web-app"}}]}}]}}"#
    )
}

fn compiled(kustomize: bool) -> Artifact {
    let release = release_json(kustomize);
    let mut files = vec![
        ("chart/Chart.yaml", "apiVersion: v2\nname: web-app\n"),
        ("chart/values.yaml", "{}\n"),
        ("chart/templates/web.yaml", "kind: Deployment\n"),
        ("release.json", release.as_str()),
    ];
    if kustomize {
        files.push((
            "kustomize/kustomization.yaml",
            "resources:\n  - helm-rendered.yaml\nnamePrefix: prod-\n",
        ));
    }
    artifact(&files)
}

fn context(dir: &Path) -> RunContext {
    RunContext {
        document: "web-app".into(),
        work_dir: dir.join("work"),
        ..RunContext::default()
    }
}

/// What `helm template` prints for the chart.
const TEMPLATED: &str = "---
# Source: web-app/templates/web_config.yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: web-config
data:
  mode: fast
---
# Source: web-app/templates/web.yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
spec:
  replicas: 2
---
# Source: web-app/templates/web_svc.yaml
apiVersion: v1
kind: Service
metadata:
  name: web
---
# Source: web-app/templates/migrate.yaml
apiVersion: batch/v1
kind: Job
metadata:
  name: migrate
  annotations:
    helm.sh/hook: pre-upgrade
";

/// What the release holds: the Deployment differs, the Service is not there
/// yet, and there is a ConfigMap the chart no longer has.
const INSTALLED: &str = "---
# Source: web-app/templates/web_config.yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: web-config
data:
  mode: fast
---
# Source: web-app/templates/old.yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: old-settings
---
# Source: web-app/templates/web.yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
spec:
  replicas: 1
";

const SERVICE: &str = r#"{"spec":{"ports":[{"port":80}]},"status":{"loadBalancer":{"ingress":[{"ip":"203.0.113.7"}]}}}"#;

/// Stand-ins that append each call to `calls` and answer from the files the
/// test wrote beside them. `status` is the release's status, or empty for a
/// release that does not exist.
fn runner(dir: &Path, status: &str, installed: &str) -> HelmRunner {
    let d = dir.display();
    fs::write(dir.join("templated"), TEMPLATED).unwrap();
    fs::write(dir.join("installed"), installed).unwrap();
    fs::write(dir.join("status"), status).unwrap();
    let helm = stand_in(
        dir,
        "helm",
        &format!(
            r#"echo "helm $@" >> "{d}/calls"
case "$1" in
template) cat "{d}/templated" ;;
status)
  if [ -s "{d}/status" ]; then echo "{{\"name\":\"web-app\",\"info\":{{\"status\":\"$(cat "{d}/status")\"}}}}"
  else echo 'Error: release: not found' >&2; exit 1; fi ;;
get) cat "{d}/installed" ;;
upgrade)
  if [ -e "{d}/fail-upgrade" ]; then echo 'Error: UPGRADE FAILED: timed out waiting for the condition' >&2; exit 1; fi
  echo 'Release "web-app" has been upgraded. Happy Helming!' ;;
esac"#
        ),
    );
    // Like the real one: reads the file the kustomization names, and
    // prefixes every name.
    let kustomize = stand_in(
        dir,
        "kustomize",
        &format!(
            r#"echo "kustomize $@" >> "{d}/calls"
sed -e '/^#/d' -e 's/^  name: /  name: prod-/' "$2/helm-rendered.yaml""#
        ),
    );
    let kubectl = stand_in(
        dir,
        "kubectl",
        &format!(
            r#"echo "kubectl $@" >> "{d}/calls"
case "$2" in
Service) echo '{SERVICE}' ;;
Deployment.v1.apps) echo '{{"status":{{"readyReplicas":2}}}}' ;;
Secret) echo '{{"data":{{"token":"aHVudGVyMg=="}}}}' ;;
*) echo "Error from server (NotFound): $2 not found" >&2; exit 1 ;;
esac"#
        ),
    );
    let path = |p: &Path| p.to_str().unwrap().to_owned();
    HelmRunner {
        helm: path(&helm),
        helm_args: vec!["--kube-context".into(), "test".into()],
        kustomize: vec![path(&kustomize), "build".into()],
        kubectl: path(&kubectl),
        kubectl_args: vec!["--context".into(), "test".into()],
    }
}

/// The calls so far, without what every one of them carries.
fn calls(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("calls"))
        .unwrap_or_default()
        .lines()
        .map(|l| {
            l.replace(" --namespace web --kube-context test", "")
                .replace(" --output json --context test", "")
        })
        .collect()
}

fn shown(changes: &[meridian_sdk::Change]) -> Vec<String> {
    changes.iter().map(|c| c.to_string()).collect()
}

#[test]
fn a_plan_for_a_new_release_creates_everything_but_the_hooks() {
    let dir = scratch(TMP, "plan-new");
    let log = KeptLog::default();
    let plan = runner(&dir, "", "")
        .plan(&context(&dir), &compiled(false), &log)
        .unwrap();
    assert_eq!(
        calls(&dir),
        [
            "helm template web-app chart",
            "helm status web-app --output json",
        ]
    );
    assert_eq!(
        shown(&plan.changes),
        [
            "create web_config: ConfigMap web-config",
            "create web: Deployment web",
            "create web_svc: Service web",
        ]
    );
    assert!(!plan.unknown);
    assert_eq!(log.text(), ["release web-app: rendering the chart"]);
    // The artifact is on disk for anyone who wants to run helm by hand.
    assert!(dir.join("work/chart/Chart.yaml").is_file());
}

#[test]
fn a_plan_for_an_installed_release_compares_with_what_helm_holds() {
    let dir = scratch(TMP, "plan-upgrade");
    let plan = runner(&dir, "deployed", INSTALLED)
        .plan(&context(&dir), &compiled(false), &KeptLog::default())
        .unwrap();
    assert_eq!(
        calls(&dir),
        [
            "helm template web-app chart",
            "helm status web-app --output json",
            "helm get manifest web-app",
        ]
    );
    assert_eq!(
        shown(&plan.changes),
        [
            "update web: Deployment web",
            "create web_svc: Service web",
            // No resource of the document accounts for it any more, so it
            // goes by the tool's own name.
            "delete ConfigMap/old-settings: ConfigMap old-settings",
        ]
    );
}

#[test]
fn a_release_that_matches_its_chart_is_in_sync_and_is_not_upgraded() {
    let dir = scratch(TMP, "in-sync");
    let helm = runner(&dir, "deployed", TEMPLATED);
    let plan = helm
        .plan(&context(&dir), &compiled(false), &KeptLog::default())
        .unwrap();
    assert!(plan.in_sync());

    let log = KeptLog::default();
    let applied = helm.apply(&context(&dir), &compiled(false), &log).unwrap();
    assert!(applied.changes.is_empty());
    assert_eq!(applied.outputs.len(), 5);
    assert!(
        !calls(&dir).iter().any(|c| c.starts_with("helm upgrade")),
        "an unchanged release gets no new revision"
    );
    assert!(log
        .text()
        .contains(&"release web-app: nothing to change".to_owned()));
}

#[test]
fn a_failed_release_is_upgraded_again_even_when_nothing_differs() {
    let dir = scratch(TMP, "failed-release");
    let helm = runner(&dir, "failed", TEMPLATED);
    let log = KeptLog::default();
    let plan = helm.plan(&context(&dir), &compiled(false), &log).unwrap();
    assert!(plan.changes.is_empty());
    assert!(
        plan.unknown,
        "what an upgrade repairs is not in the manifests"
    );
    assert!(!plan.in_sync());
    assert!(log.text().contains(
        &"release web-app: its status is failed, so an apply will upgrade it again".to_owned()
    ));

    helm.apply(&context(&dir), &compiled(false), &KeptLog::default())
        .unwrap();
    assert!(calls(&dir).iter().any(|c| c.starts_with("helm upgrade")));
}

#[test]
fn an_apply_installs_with_the_releases_settings_and_reads_the_outputs() {
    let dir = scratch(TMP, "apply");
    let log = KeptLog::default();
    let applied = runner(&dir, "", "")
        .apply(&context(&dir), &compiled(false), &log)
        .unwrap();
    assert_eq!(
        calls(&dir),
        [
            "helm template web-app chart",
            "helm status web-app --output json",
            "helm upgrade --install web-app chart --create-namespace --wait --timeout 5m",
            // Each object is read once, however many outputs use it.
            "kubectl get Service web --namespace web",
            "kubectl get Deployment.v1.apps web --namespace web",
            "kubectl get Secret keys --namespace web",
        ]
    );
    assert_eq!(applied.changes.len(), 3);
    assert_eq!(
        log.text(),
        [
            "release web-app: rendering the chart",
            "release web-app: installing",
            "Release \"web-app\" has been upgraded. Happy Helming!",
        ]
    );

    let outputs: Vec<(&str, &Value, bool)> = applied
        .outputs
        .iter()
        .map(|o| (o.name.as_str(), &o.value, o.sensitive))
        .collect();
    assert_eq!(
        outputs,
        [
            ("address", &Value::String("203.0.113.7".into()), false),
            (
                "url",
                &Value::String("http://203.0.113.7:80/".into()),
                false
            ),
            // One reference alone keeps the type the cluster gave it.
            ("replicas", &Value::Int(2), false),
            ("token", &Value::String("aHVudGVyMg==".into()), true),
            ("name", &Value::String("web-app".into()), false),
        ]
    );
    // A sensitive output does not print its value.
    assert!(!format!("{:?}", applied.outputs).contains("aHVudGVyMg"));
}

#[test]
fn an_overlay_is_applied_to_what_helm_renders_and_helm_installs_the_result() {
    let dir = scratch(TMP, "overlay");
    let log = KeptLog::default();
    let applied = runner(&dir, "", "")
        .apply(&context(&dir), &compiled(true), &log)
        .unwrap();
    assert_eq!(
        calls(&dir)[..4],
        [
            "helm template web-app chart",
            "kustomize build kustomize",
            "helm status web-app --output json",
            "helm upgrade --install web-app patched --create-namespace --wait --timeout 5m",
        ]
    );
    // What is reported is what the overlay made of each object, traced back
    // to the resource that declared it.
    assert_eq!(
        shown(&applied.changes),
        [
            "create web_config: ConfigMap prod-web-config",
            "create web: Deployment prod-web",
            "create web_svc: Service prod-web",
        ]
    );
    assert!(log
        .text()
        .contains(&"release web-app: applying the Kustomize overlay".to_owned()));

    let work = dir.join("work");
    assert_eq!(
        fs::read_to_string(work.join("kustomize/helm-rendered.yaml")).unwrap(),
        TEMPLATED
    );
    assert_eq!(
        fs::read_to_string(work.join("patched/Chart.yaml")).unwrap(),
        "apiVersion: v2\nname: web-app\ndescription: What the compiled chart renders, patched by its Kustomize overlay.\ntype: application\nversion: 1.4.0\n"
    );
    let patched = fs::read_to_string(work.join("patched/templates/manifests.yaml")).unwrap();
    assert!(patched.contains("  name: prod-web-config\n"), "{patched}");
    // The objects are read where the overlay put them.
    assert!(calls(&dir).contains(&"kubectl get Service prod-web --namespace prod".to_owned()));
}

#[test]
fn what_kustomize_prints_is_installed_as_it_stands() {
    let dir = scratch(TMP, "overlay-braces");
    let helm = runner(&dir, "", "");
    fs::write(
        dir.join("templated"),
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: tmpl\ndata:\n  page: '{{ .Title }}'\n",
    )
    .unwrap();
    let rendered = helm
        .render(&context(&dir), &compiled(true), &KeptLog::default())
        .unwrap();
    assert!(rendered.contains("page: '{{ .Title }}'"), "{rendered}");
    // In the chart Helm installs, the braces are escaped so that they are
    // still braces once Helm has rendered it.
    let patched = fs::read_to_string(dir.join("work/patched/templates/manifests.yaml")).unwrap();
    assert!(
        patched.contains("page: '{{ `{{` }} .Title }}'"),
        "{patched}"
    );
}

#[test]
fn a_failed_install_says_what_helm_said() {
    let dir = scratch(TMP, "failed-install");
    let helm = runner(&dir, "", "");
    fs::write(dir.join("fail-upgrade"), "").unwrap();
    let err = helm
        .apply(&context(&dir), &compiled(false), &KeptLog::default())
        .unwrap_err();
    assert!(
        err.msg.starts_with("helm could not install the release"),
        "{}",
        err.msg
    );
    assert_eq!(err.exit_code, Some(1));
    assert!(err.output.contains("timed out waiting for the condition"));
    assert!(err.command.contains("upgrade --install web-app chart"));
}

#[test]
fn an_output_the_cluster_has_not_filled_in_yet_says_so() {
    let dir = scratch(TMP, "pending-output");
    let helm = runner(&dir, "deployed", TEMPLATED);
    stand_in(
        &dir,
        "kubectl",
        "echo '{\"spec\":{\"ports\":[{\"port\":80}]},\"status\":{\"loadBalancer\":{}}}'",
    );
    let err = helm
        .apply(&context(&dir), &compiled(false), &KeptLog::default())
        .unwrap_err();
    assert!(
        err.msg.starts_with(
            "output \"address\": the Service web has no status.loadBalancer.ingress yet"
        ),
        "{}",
        err.msg
    );
    assert!(err.msg.contains("wait: true"), "{}", err.msg);
}

#[test]
fn a_tool_that_is_missing_is_named() {
    let dir = scratch(TMP, "missing-tool");
    let mut helm = runner(&dir, "", "");
    helm.kustomize = vec!["meridian-no-such-kustomize".into(), "build".into()];
    let err = helm
        .plan(&context(&dir), &compiled(true), &KeptLog::default())
        .unwrap_err();
    assert!(
        err.msg
            .starts_with("could not run meridian-no-such-kustomize: it is not installed"),
        "{}",
        err.msg
    );
}

#[test]
fn an_artifact_from_another_plugin_is_refused() {
    let dir = scratch(TMP, "foreign");
    let helm = runner(&dir, "", "");
    let err = helm
        .plan(
            &context(&dir),
            &artifact(&[("main.tf.json", "{}")]),
            &KeptLog::default(),
        )
        .unwrap_err();
    assert!(
        err.msg.contains("was not compiled by this plugin"),
        "{}",
        err.msg
    );
    assert!(calls(&dir).is_empty());

    let err = helm
        .plan(
            &context(&dir),
            &artifact(&[("chart/Chart.yaml", "x"), ("release.json", "{}")]),
            &KeptLog::default(),
        )
        .unwrap_err();
    assert_eq!(err.msg, "release.json is not one this plugin compiled");
}

#[test]
fn outputs_need_an_earlier_apply() {
    let dir = scratch(TMP, "outputs");
    let helm = runner(&dir, "deployed", TEMPLATED);
    let err = helm.outputs(&context(&dir)).unwrap_err();
    assert_eq!(
        err.msg,
        "nothing has been applied for this document yet, so it has no outputs"
    );
    fs::create_dir_all(dir.join("work")).unwrap();
    fs::write(dir.join("work/release.json"), release_json(false)).unwrap();
    let outputs = helm.outputs(&context(&dir)).unwrap();
    assert_eq!(outputs[0].name, "address");
    assert_eq!(outputs[0].value, Value::String("203.0.113.7".into()));
}
