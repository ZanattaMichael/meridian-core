//! The results as JUnit XML, which every pipeline engine shows as tests.

use crate::run::{FileResult, Status};

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            // XML 1.0 cannot carry these at all; a report must still be
            // readable when a tool prints one.
            c if (c as u32) < 0x20 && c != '\n' && c != '\t' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// One row of the report: a case, or one phase of a lifecycle case.
struct Row<'a> {
    name: String,
    status: Status,
    failures: &'a [String],
    millis: u128,
}

fn rows(file: &FileResult) -> Vec<Row<'_>> {
    file.cases
        .iter()
        .flat_map(|case| {
            if case.phases.is_empty() {
                vec![Row {
                    name: case.name.clone(),
                    status: case.status,
                    failures: &case.failures,
                    millis: case.millis,
                }]
            } else {
                case.phases
                    .iter()
                    .map(|phase| Row {
                        name: format!("{}: {}", case.name, phase.name),
                        status: phase.status,
                        failures: &phase.failures,
                        millis: phase.millis,
                    })
                    .collect()
            }
        })
        .collect()
}

/// One `testsuite` per test document, one `testcase` per case, or per phase
/// of a lifecycle case (`<case>: <phase>`); a failed one carries a
/// `failure` with every failure's text, one that could not run an `error`,
/// a phase that did not run `skipped`.
pub fn junit(results: &[FileResult]) -> String {
    let all: Vec<Vec<Row<'_>>> = results.iter().map(rows).collect();
    let count =
        |rows: &[Row<'_>], status: Status| rows.iter().filter(|r| r.status == status).count();
    let flat: Vec<&Row<'_>> = all.iter().flatten().collect();
    let total = flat.len();
    let failures = flat.iter().filter(|r| r.status == Status::Failed).count();
    let errors = flat.iter().filter(|r| r.status == Status::Error).count();
    let skipped = flat.iter().filter(|r| r.status == Status::Skipped).count();
    let seconds = |ms: u128| format!("{}.{:03}", ms / 1000, ms % 1000);
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str(&format!(
        "<testsuites name=\"meridian test\" tests=\"{total}\" failures=\"{failures}\" errors=\"{errors}\" skipped=\"{skipped}\">\n"
    ));
    for (file, rows) in results.iter().zip(&all) {
        let time: u128 = file.cases.iter().map(|c| c.millis).sum();
        out.push_str(&format!(
            "  <testsuite name=\"{}\" file=\"{}\" tests=\"{}\" failures=\"{}\" errors=\"{}\" skipped=\"{}\" time=\"{}\">\n",
            escape(&file.name),
            escape(&file.path.display().to_string()),
            rows.len(),
            count(rows, Status::Failed),
            count(rows, Status::Error),
            count(rows, Status::Skipped),
            seconds(time)
        ));
        for row in rows {
            out.push_str(&format!(
                "    <testcase classname=\"{}\" name=\"{}\" time=\"{}\"",
                escape(&file.name),
                escape(&row.name),
                seconds(row.millis)
            ));
            let first = row.failures.first().map(String::as_str).unwrap_or("");
            match row.status {
                Status::Passed => out.push_str("/>\n"),
                Status::Skipped => out.push_str(&format!(
                    ">\n      <skipped message=\"{}\"/>\n    </testcase>\n",
                    escape(first)
                )),
                Status::Failed | Status::Error => {
                    let tag = if row.status == Status::Failed {
                        "failure"
                    } else {
                        "error"
                    };
                    out.push_str(&format!(
                        ">\n      <{tag} message=\"{}\">{}</{tag}>\n    </testcase>\n",
                        escape(first),
                        escape(&row.failures.join("\n"))
                    ));
                }
            }
        }
        out.push_str("  </testsuite>\n");
    }
    out.push_str("</testsuites>\n");
    out
}
