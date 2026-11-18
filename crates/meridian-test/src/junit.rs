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

/// One `testsuite` per test document, one `testcase` per case; a failed
/// case carries a `failure` with every failure's text, a case that could
/// not run an `error`.
pub fn junit(results: &[FileResult]) -> String {
    let total: usize = results.iter().map(|f| f.cases.len()).sum();
    let failures = results
        .iter()
        .flat_map(|f| &f.cases)
        .filter(|c| c.status == Status::Failed)
        .count();
    let errors = results
        .iter()
        .flat_map(|f| &f.cases)
        .filter(|c| c.status == Status::Error)
        .count();
    let seconds = |ms: u128| format!("{}.{:03}", ms / 1000, ms % 1000);
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str(&format!(
        "<testsuites name=\"meridian test\" tests=\"{total}\" failures=\"{failures}\" errors=\"{errors}\">\n"
    ));
    for file in results {
        let failed = file
            .cases
            .iter()
            .filter(|c| c.status == Status::Failed)
            .count();
        let errored = file
            .cases
            .iter()
            .filter(|c| c.status == Status::Error)
            .count();
        let time: u128 = file.cases.iter().map(|c| c.millis).sum();
        out.push_str(&format!(
            "  <testsuite name=\"{}\" file=\"{}\" tests=\"{}\" failures=\"{failed}\" errors=\"{errored}\" time=\"{}\">\n",
            escape(&file.name),
            escape(&file.path.display().to_string()),
            file.cases.len(),
            seconds(time)
        ));
        for case in &file.cases {
            out.push_str(&format!(
                "    <testcase classname=\"{}\" name=\"{}\" time=\"{}\"",
                escape(&file.name),
                escape(&case.name),
                seconds(case.millis)
            ));
            match case.status {
                Status::Passed => out.push_str("/>\n"),
                Status::Failed | Status::Error => {
                    let tag = if case.status == Status::Failed {
                        "failure"
                    } else {
                        "error"
                    };
                    let first = case.failures.first().map(String::as_str).unwrap_or("");
                    out.push_str(&format!(
                        ">\n      <{tag} message=\"{}\">{}</{tag}>\n    </testcase>\n",
                        escape(first),
                        escape(&case.failures.join("\n"))
                    ));
                }
            }
        }
        out.push_str("  </testsuite>\n");
    }
    out.push_str("</testsuites>\n");
    out
}
