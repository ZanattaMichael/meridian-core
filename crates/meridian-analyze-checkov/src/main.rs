//! `meridian-analyze-checkov`: see the library for what it does.

fn main() -> std::process::ExitCode {
    meridian_analyze::main_with(
        "meridian-analyze-checkov",
        meridian_analyze_checkov::analyze,
    )
}
