//! `meridian-analyze-trivy`: see the library for what it does.

fn main() -> std::process::ExitCode {
    meridian_analyze::main_with("meridian-analyze-trivy", meridian_analyze_trivy::analyze)
}
