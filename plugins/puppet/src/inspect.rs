//! Reading one resource for `meridian inspect`: its type and name from the
//! compiled manifest, and what `puppet resource` prints for it.

use meridian_import::yaml::{self, Rules};
use meridian_sdk::{Data, Value};

/// The type and name `puppet resource` takes for the declaration titled
/// `title` in `manifest`: the namevar's value when the declaration sets it,
/// otherwise the title.
pub fn declared(manifest: &str, title: &str) -> Option<(String, String)> {
    let opening = format!(" {{ '{title}':");
    let mut lines = manifest.lines();
    let r#type = lines.by_ref().find_map(|line| {
        let line = line.trim_start();
        let at = line.find(&opening)?;
        Some(line[..at].to_owned())
    })?;
    let namevar = match r#type.as_str() {
        "file" => "path",
        "exec" => "command",
        _ => "name",
    };
    let name = lines
        .take_while(|line| line.trim() != "}")
        .find_map(|line| {
            let (key, value) = line.trim().split_once("=>")?;
            (key.trim() == namevar).then(|| literal(value.trim().trim_end_matches(',')))?
        })
        .unwrap_or_else(|| title.to_owned());
    Some((r#type, name))
}

/// A quoted Puppet string, unquoted; `None` for anything else.
fn literal(text: &str) -> Option<String> {
    if let Some(inner) = text.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')) {
        return Some(inner.replace("\\'", "'").replace("\\\\", "\\"));
    }
    let inner = text.strip_prefix('"')?.strip_suffix('"')?;
    // An interpolated value is only known on the node.
    if inner.contains('$') {
        return None;
    }
    decode_json_string(inner)
}

fn decode_json_string(inner: &str) -> Option<String> {
    match meridian_sdk::decode_value(format!("\"{inner}\"").as_bytes()) {
        Ok(Value::String(s)) => Some(s),
        _ => None,
    }
}

/// What `puppet resource TYPE NAME --to_yaml` printed, as found and
/// properties. A file's content is printed as its checksum, which says
/// nothing comparable, so it is left out.
pub fn read_resource(yaml_text: &str) -> Result<(bool, Data), String> {
    let documents =
        yaml::read("puppet resource", yaml_text, Rules::Yaml11).map_err(|e| e.to_string())?;
    let resource = documents
        .first()
        .and_then(|d| d.as_map())
        .and_then(|types| types.first())
        .and_then(|(_, by_name)| by_name.as_map())
        .and_then(|names| names.first())
        .map(|(_, node)| node.value());
    let Some(Value::Map(properties)) = resource else {
        return Ok((false, Data::new()));
    };
    let ensure = properties
        .get("ensure")
        .and_then(Value::as_str)
        .unwrap_or("");
    let found = !matches!(ensure, "absent" | "purged");
    let actual = properties
        .into_iter()
        .filter(|(k, v)| {
            let checksum = v
                .as_str()
                .is_some_and(|s| s.starts_with("{sha256}") || s.starts_with("{md5}"));
            !(k == "content" && checksum)
        })
        .collect();
    Ok((found, actual))
}

/// The resource `bolt command run` printed, from its JSON report.
pub fn bolt_stdout(json: &str) -> Result<String, String> {
    let report = meridian_sdk::decode_value(json.trim().as_bytes())?;
    let items = match report.as_map() {
        Some(map) => map
            .get("items")
            .and_then(Value::as_list)
            .unwrap_or(&[])
            .to_vec(),
        None => report.as_list().unwrap_or(&[]).to_vec(),
    };
    let value = items
        .first()
        .and_then(Value::as_map)
        .and_then(|i| i.get("value"))
        .and_then(Value::as_map)
        .ok_or("bolt printed no result")?;
    if let Some(error) = value.get("_error").and_then(Value::as_map) {
        return Err(error
            .get("msg")
            .and_then(Value::as_str)
            .unwrap_or("bolt could not run the command")
            .to_owned());
    }
    Ok(value
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = "class meridian_web {
  file { 'site_conf':
    ensure  => file,
    content => \"listen 80;\\n\",
    path    => '/etc/nginx/conf.d/site.conf',
  }

  service { 'nginx_svc':
    ensure => running,
  }
}
";

    #[test]
    fn the_type_and_namevar_come_from_the_declaration() {
        assert_eq!(
            declared(MANIFEST, "site_conf"),
            Some(("file".to_owned(), "/etc/nginx/conf.d/site.conf".to_owned()))
        );
        assert_eq!(
            declared(MANIFEST, "nginx_svc"),
            Some(("service".to_owned(), "nginx_svc".to_owned()))
        );
        assert_eq!(declared(MANIFEST, "nothing"), None);
    }

    #[test]
    fn a_resource_is_read_with_its_checksum_left_out() {
        let (found, actual) = read_resource(
            "---\nfile:\n  \"/etc/hostname\":\n    ensure: file\n    content: \"{sha256}42\"\n    mode: '0644'\n",
        )
        .unwrap();
        assert!(found);
        assert_eq!(actual.get("mode"), Some(&Value::from("0644")));
        assert!(!actual.contains_key("content"));
        let (found, _) = read_resource("---\nfile:\n  \"/nope\":\n    ensure: absent\n").unwrap();
        assert!(!found);
    }
}
