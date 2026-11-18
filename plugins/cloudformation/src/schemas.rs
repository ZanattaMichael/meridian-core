//! Reading the resource provider schemas CloudFormation publishes.
//!
//! CloudFormation describes every resource type in its registry with a
//! JSON Schema (the resource provider schema), and publishes the whole
//! registry of a region as one zip, so an editor can know `AWS::S3::Bucket`
//! without an account or the `aws` command. This half of the plugin fetches
//! that zip for the region the `stack` resource names, reads the schema of
//! each type the documents use in full, and lists every other type of the
//! registry by name, so completion offers it; the next sync after a
//! document starts using one describes it in full.
//!
//! A type that is not in the public registry, a private or third-party
//! type the account activated, is asked of `aws cloudformation
//! describe-type`, which needs the command and credentials; a type that
//! cannot be found either way is a note.

use meridian_sdk::schema::params_from_json_schema;
use meridian_sdk::tool::Tool;
use meridian_sdk::{
    decode_value, fetch, settings, Log, ParamSchema, ResourceGraph, ResourceSchema, RunError,
    SchemaExport, SchemaRequest, SchemaSource, Value,
};

use crate::runner::settings as runner_settings;

/// The region whose registry is read when no `stack` names one and the
/// environment does not either.
pub const DEFAULT_REGION: &str = "us-east-1";

/// The name of the resource type that names the region.
const STACK_TYPE: &str = "stack";

/// The file the registry is kept as in the work directory.
const ZIP_NAME: &str = "CloudformationSchema.zip";

/// Where a region's registry is published.
pub fn registry_url(region: &str) -> String {
    format!("https://schema.cloudformation.{region}.amazonaws.com/CloudformationSchema.zip")
}

/// Reads the published registry of the documents' region.
pub struct CloudFormationSchemas {
    /// The `aws` program, for a private type.
    pub program: String,
    /// Arguments given to every `aws` call, from the environment.
    pub extra_args: Vec<String>,
    /// The URL of the zip, with `{region}` where the region goes. Changed
    /// only by tests.
    pub url: String,
}

impl CloudFormationSchemas {
    /// The source as the environment configures it, the same settings the
    /// runner reads.
    pub fn from_env() -> Self {
        let declared = runner_settings();
        let setting = |name: &str| settings::find(&declared, name);
        CloudFormationSchemas {
            program: settings::text(setting("program")),
            extra_args: settings::args(setting("args")),
            url: registry_url("{region}"),
        }
    }

    /// The region the registry is read for: the first `stack` resource's
    /// `region`, else `--region` among the extra arguments, else what the
    /// `aws` command itself would use from the environment, else
    /// `us-east-1`.
    pub fn region(&self, graphs: &[ResourceGraph]) -> String {
        if let Some(region) = graphs
            .iter()
            .flat_map(|g| g.resources.iter())
            .filter(|r| r.r#type == STACK_TYPE)
            .find_map(|r| r.params.get("region").and_then(Value::as_str))
            .filter(|r| !r.is_empty())
        {
            return region.to_owned();
        }
        if let Some(at) = self.extra_args.iter().position(|a| a == "--region") {
            if let Some(region) = self.extra_args.get(at + 1) {
                return region.clone();
            }
        }
        for variable in ["AWS_REGION", "AWS_DEFAULT_REGION"] {
            if let Some(region) = std::env::var(variable)
                .ok()
                .filter(|v| !v.trim().is_empty())
            {
                return region.trim().to_owned();
            }
        }
        DEFAULT_REGION.to_owned()
    }
}

/// A region is lower-case letters, digits and dashes; anything else would
/// make a URL that is not the registry's.
fn is_region(region: &str) -> bool {
    !region.is_empty()
        && region
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl SchemaSource for CloudFormationSchemas {
    fn export(&self, request: &SchemaRequest, log: &dyn Log) -> Result<SchemaExport, RunError> {
        let region = self.region(&request.graphs);
        if !is_region(&region) {
            return Err(RunError::new(format!(
                "{region:?} is not a region name, so its registry cannot be fetched"
            )));
        }
        let url = self.url.replace("{region}", &region);
        let fetched = fetch(
            &url,
            &request.work_dir.join(&region),
            ZIP_NAME,
            &format!("the CloudFormation resource schemas of {region}"),
            log,
        )?;
        let mut export = SchemaExport::new("cloudformation-registry", &fetched.pin());
        export.pin(&region, &fetched.pin());
        if let Some(note) = &fetched.note {
            export.note(note.clone());
        }
        let wanted: Vec<&str> = request
            .types
            .iter()
            .map(String::as_str)
            .filter(|t| *t != STACK_TYPE)
            .collect();
        let source = format!("{region} registry {}", fetched.pin());
        let mut read = Read::default();
        crate::zip::read_entries(&fetched.bytes, |entry| {
            if !entry.name.ends_with(".json") {
                return Ok(());
            }
            match decode_value(&entry.bytes) {
                Ok(document) => read.add(&document, &wanted, &source),
                Err(e) => read.unreadable.push(format!("{}: {e}", entry.name)),
            }
            Ok(())
        })
        .map_err(|e| {
            RunError::new(format!(
                "the registry zip of {region} could not be read: {e}"
            ))
        })?;
        if !read.unreadable.is_empty() {
            export.note(format!(
                "{} of the registry could not be read: {}",
                count(read.unreadable.len(), "schema"),
                read.unreadable.join("; ")
            ));
        }
        export.schemas = read.schemas;
        if read.named > 0 {
            export.note(format!(
                "{} listed by name only; a type is described in full once a document uses it \
                 and the schemas are synced again",
                count(read.named, "other type")
            ));
        }

        // What the public registry does not have may be a private or
        // third-party type the account activated.
        for wanted in wanted {
            if export.schemas.iter().any(|s| s.r#type == wanted) {
                continue;
            }
            match self.describe_private(wanted, &region, log) {
                Ok(Some(schema)) => export.schemas.push(schema),
                Ok(None) => export.note(format!(
                    "{wanted} is not in the {region} registry, and {} describe-type does not \
                     know it either",
                    self.program
                )),
                Err(why) => export.note(format!(
                    "{wanted} is not in the {region} registry; asking {} describe-type failed: {why}",
                    self.program
                )),
            }
        }
        export.tidy();
        Ok(export)
    }
}

impl CloudFormationSchemas {
    /// `aws cloudformation describe-type` for a type outside the public
    /// registry. `Ok(None)` when the command ran and does not know it.
    fn describe_private(
        &self,
        type_name: &str,
        region: &str,
        log: &dyn Log,
    ) -> Result<Option<ResourceSchema>, String> {
        let finished = Tool::new(&self.program)
            .args([
                "cloudformation",
                "describe-type",
                "--type",
                "RESOURCE",
                "--type-name",
                type_name,
                "--region",
                region,
                "--output",
                "json",
            ])
            .args(self.extra_args.iter().cloned())
            .capture_stdout()
            .run(log)
            .map_err(|e| e.msg)?;
        if !finished.success() {
            let tail = finished.tail().trim().to_owned();
            return if tail.contains("TypeNotFoundException") {
                Ok(None)
            } else {
                Err(tail)
            };
        }
        let answer = decode_value(finished.stdout.as_bytes()).map_err(|e| e.to_string())?;
        let text = answer
            .as_map()
            .and_then(|m| m.get("Schema"))
            .and_then(Value::as_str)
            .ok_or_else(|| "the answer has no Schema".to_owned())?;
        let document = decode_value(text.as_bytes()).map_err(|e| e.to_string())?;
        let mut schema =
            read_type(&document, true).ok_or_else(|| "the schema has no typeName".to_owned())?;
        schema.source = format!("{region} private registry");
        Ok(Some(schema))
    }
}

/// What was read from the registry.
#[derive(Debug, Default)]
struct Read {
    schemas: Vec<ResourceSchema>,
    /// How many types were listed by name only.
    named: usize,
    unreadable: Vec<String>,
}

impl Read {
    fn add(&mut self, document: &Value, wanted: &[&str], source: &str) {
        let name = type_name(document);
        let full = wanted.contains(&name.as_str());
        let Some(mut schema) = read_type(document, full) else {
            return;
        };
        if !full {
            self.named += 1;
        }
        schema.source = source.to_owned();
        self.schemas.push(schema);
    }
}

fn type_name(document: &Value) -> String {
    document
        .as_map()
        .and_then(|m| m.get("typeName"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// A JSON Pointer under `/properties`, as the schema's property lists
/// write them, as a path of param names.
fn property_path(pointer: &str) -> Option<Vec<&str>> {
    let rest = pointer.strip_prefix("/properties/")?;
    Some(
        rest.split("/properties/")
            .flat_map(|part| part.split('/'))
            .filter(|p| !p.is_empty() && *p != "*")
            .collect(),
    )
}

fn pointers<'a>(document: &'a Value, key: &str) -> Vec<&'a str> {
    document
        .as_map()
        .and_then(|m| m.get(key))
        .and_then(Value::as_list)
        .map(|l| l.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

fn param_at<'a>(params: &'a mut [ParamSchema], path: &[&str]) -> Option<&'a mut ParamSchema> {
    let (first, rest) = path.split_first()?;
    let param = params.iter_mut().find(|p| p.name == *first)?;
    if rest.is_empty() {
        Some(param)
    } else {
        param_at(&mut param.params, rest)
    }
}

/// One resource provider schema as a resource schema: in full, with its
/// properties as params and the read-only ones computed, or by name.
pub(crate) fn read_type(document: &Value, full: bool) -> Option<ResourceSchema> {
    let map = document.as_map()?;
    let name = type_name(document);
    if name.is_empty() {
        return None;
    }
    let description = first_paragraph(map.get("description").and_then(Value::as_str).unwrap_or(""));
    if !full {
        return Some(ResourceSchema::open(&name, &description));
    }
    let definitions = map.get("definitions").and_then(Value::as_map);
    let resolve = |reference: &str| -> Option<&Value> {
        let key = reference.strip_prefix("#/definitions/")?;
        definitions?.get(key)
    };
    let (mut params, open) = params_from_json_schema(document, &resolve);
    // The description a property carries is often several paragraphs of
    // the user guide; the first says what it is.
    for param in &mut params {
        param.description = first_paragraph(&param.description);
    }
    for pointer in pointers(document, "readOnlyProperties") {
        if let Some(param) = property_path(pointer).and_then(|path| param_at(&mut params, &path)) {
            param.computed = true;
        }
    }
    for pointer in pointers(document, "createOnlyProperties") {
        if let Some(param) = property_path(pointer).and_then(|path| param_at(&mut params, &path)) {
            if !param.description.contains("replaces the resource") {
                param.description = format!(
                    "{}{}Set when the resource is created; changing it replaces the resource.",
                    param.description.trim_end(),
                    if param.description.is_empty() {
                        ""
                    } else {
                        " "
                    }
                );
            }
        }
    }
    for pointer in pointers(document, "writeOnlyProperties") {
        if let Some(param) = property_path(pointer).and_then(|path| param_at(&mut params, &path)) {
            param.sensitive = true;
        }
    }
    params.sort_by(|a, b| a.name.cmp(&b.name));
    let mut schema = ResourceSchema::closed(&name, &description).with_params(params);
    schema.open = open;
    Some(schema)
}

/// The registry writes a description as the user guide's paragraphs,
/// separated by a newline and a space; the first says what the thing is.
fn first_paragraph(text: &str) -> String {
    meridian_sdk::fetch::first_paragraph(text.split("\n ").next().unwrap_or(text))
}

fn count(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `aws-s3-bucket.json` from the registry, cut down to the shapes the
    /// reader has to handle.
    const BUCKET: &str = r##"{
  "typeName": "AWS::S3::Bucket",
  "description": "The ``AWS::S3::Bucket`` resource creates an Amazon S3 bucket in the same AWS Region where you create the AWS CloudFormation stack.\n To control how AWS CloudFormation handles the bucket when the stack is deleted, you can set a deletion policy for your bucket.",
  "definitions": {
    "Tag": {
      "type": "object",
      "additionalProperties": false,
      "properties": {
        "Key": {"type": "string", "description": "Name of the object key."},
        "Value": {"type": "string", "description": "Value of the tag."}
      },
      "required": ["Value", "Key"]
    },
    "VersioningConfiguration": {
      "type": "object",
      "additionalProperties": false,
      "properties": {
        "Status": {"type": "string", "enum": ["Enabled", "Suspended"], "default": "Suspended", "description": "The versioning state of the bucket."}
      },
      "required": ["Status"]
    }
  },
  "properties": {
    "BucketName": {"type": "string", "description": "A name for the bucket. If you don't specify a name, AWS CloudFormation generates a unique ID.\n The name must be unique."},
    "Tags": {"type": "array", "insertionOrder": false, "items": {"$ref": "#/definitions/Tag"}, "description": "An arbitrary set of tags (key-value pairs) for this S3 bucket."},
    "VersioningConfiguration": {"$ref": "#/definitions/VersioningConfiguration"},
    "Arn": {"type": "string", "description": ""},
    "DomainName": {"type": "string", "description": ""},
    "MetadataTableConfiguration": {
      "type": "object",
      "additionalProperties": false,
      "properties": {
        "S3TablesDestination": {
          "type": "object",
          "additionalProperties": false,
          "properties": {
            "TableBucketArn": {"type": "string"},
            "TableArn": {"type": "string"}
          }
        }
      }
    }
  },
  "additionalProperties": false,
  "readOnlyProperties": ["/properties/Arn", "/properties/DomainName", "/properties/MetadataTableConfiguration/S3TablesDestination/TableArn"],
  "createOnlyProperties": ["/properties/BucketName"],
  "writeOnlyProperties": ["/properties/Tags/*/Value"],
  "primaryIdentifier": ["/properties/BucketName"]
}"##;

    #[test]
    fn a_registry_schema_becomes_a_resource_schema() {
        let document = decode_value(BUCKET.as_bytes()).unwrap();
        let schema = read_type(&document, true).unwrap();
        assert_eq!(schema.r#type, "AWS::S3::Bucket");
        assert!(schema
            .description
            .starts_with("The ``AWS::S3::Bucket`` resource creates"));
        assert!(
            !schema.description.contains("deletion policy"),
            "the first paragraph only: {}",
            schema.description
        );
        assert!(!schema.open);
        let p = |name: &str| schema.find_param(name).unwrap_or_else(|| panic!("{name}"));
        assert!(p("Arn").computed && p("DomainName").computed);
        assert!(!p("BucketName").computed);
        assert!(
            p("BucketName")
                .description
                .ends_with("changing it replaces the resource."),
            "{}",
            p("BucketName").description
        );
        assert!(
            p("BucketName")
                .description
                .starts_with("A name for the bucket."),
            "{}",
            p("BucketName").description
        );
        // A nested read-only property is found through the pointer.
        let table = p("MetadataTableConfiguration")
            .find_param("S3TablesDestination")
            .unwrap();
        assert!(table.find_param("TableArn").unwrap().computed);
        assert!(!table.find_param("TableBucketArn").unwrap().computed);
        // A $ref into definitions is followed, into a list's items too.
        let tags = p("Tags");
        assert_eq!(tags.kind, meridian_sdk::ParamKind::List);
        assert!(tags.find_param("Key").unwrap().required);
        assert!(
            tags.find_param("Value").unwrap().sensitive,
            "write-only through a wildcard"
        );
        assert_eq!(
            p("VersioningConfiguration")
                .find_param("Status")
                .unwrap()
                .values,
            ["Enabled", "Suspended"]
        );
        // Sorted, so the cache is stable.
        let names: Vec<&str> = schema.params.iter().map(|p| p.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn a_type_the_documents_do_not_use_is_listed_by_name() {
        let document = decode_value(BUCKET.as_bytes()).unwrap();
        let schema = read_type(&document, false).unwrap();
        assert!(schema.open && schema.params.is_empty());
        assert!(schema.description.starts_with("The ``AWS::S3::Bucket``"));
        assert!(read_type(&decode_value(b"{}").unwrap(), true).is_none());
    }

    #[test]
    fn the_region_comes_from_the_stack_then_the_arguments_then_the_environment() {
        let mut source = CloudFormationSchemas {
            program: "aws".to_owned(),
            extra_args: vec![],
            url: registry_url("{region}"),
        };
        let mut graph = ResourceGraph::default();
        graph.resources.push(meridian_sdk::Resource {
            r#type: "stack".to_owned(),
            params: [("region".to_owned(), Value::String("eu-west-1".to_owned()))]
                .into_iter()
                .collect(),
            ..Default::default()
        });
        assert_eq!(source.region(std::slice::from_ref(&graph)), "eu-west-1");
        source.extra_args = vec!["--region".to_owned(), "ap-south-1".to_owned()];
        assert_eq!(source.region(&[]), "ap-south-1");
        source.extra_args.clear();
        let region = source.region(&[]);
        assert!(
            region == DEFAULT_REGION
                || std::env::var("AWS_REGION").is_ok()
                || std::env::var("AWS_DEFAULT_REGION").is_ok()
        );
        assert!(is_region("us-gov-west-1") && !is_region("Us/East") && !is_region(""));
        assert_eq!(
            registry_url("eu-west-1"),
            "https://schema.cloudformation.eu-west-1.amazonaws.com/CloudformationSchema.zip"
        );
    }

    #[test]
    fn a_zipped_registry_is_read_in_full_for_used_types_and_by_name_for_the_rest() {
        let other = r##"{"typeName": "AWS::SQS::Queue", "description": "A queue.", "properties": {"QueueName": {"type": "string"}}, "additionalProperties": false}"##;
        let archive = crate::zip::tests::stored_archive(&[
            ("aws-s3-bucket.json", BUCKET.as_bytes()),
            ("aws-sqs-queue.json", other.as_bytes()),
            ("README.txt", b"not a schema"),
        ]);
        let mut read = Read::default();
        crate::zip::read_entries(&archive, |entry| {
            if entry.name.ends_with(".json") {
                read.add(
                    &decode_value(&entry.bytes).unwrap(),
                    &["AWS::S3::Bucket"],
                    "us-east-1 registry sha256:abc",
                );
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(read.named, 1);
        assert_eq!(read.schemas.len(), 2);
        let bucket = read
            .schemas
            .iter()
            .find(|s| s.r#type == "AWS::S3::Bucket")
            .unwrap();
        assert!(!bucket.open && !bucket.params.is_empty());
        assert_eq!(bucket.source, "us-east-1 registry sha256:abc");
        let queue = read
            .schemas
            .iter()
            .find(|s| s.r#type == "AWS::SQS::Queue")
            .unwrap();
        assert!(queue.open && queue.params.is_empty());
        assert_eq!(queue.description, "A queue.");
    }
}
