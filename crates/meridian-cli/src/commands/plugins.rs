//! `meridian plugins` and `meridian import`, and loading the plugin registry.

use crate::*;

/// The directories plugins, and backend programs, are looked for in.
pub(crate) fn plugin_dirs(plugins: &PluginDirs) -> Vec<PathBuf> {
    if plugins.dirs.is_empty() {
        own_directory().into_iter().collect()
    } else {
        plugins.dirs.clone()
    }
}

/// The project's settings for its targets, from `spec.settings` in the
/// `meridian.yaml` of the current directory. A directory with no manifest
/// has none; a manifest that does not read stops the command, because a
/// project whose settings are ignored would run its tools wrongly.
pub(crate) fn project_settings() -> Result<BTreeMap<String, BTreeMap<String, String>>, Failure> {
    let dir = Path::new(".");
    if !dir.join(meridian_module::MANIFEST_FILE).is_file() {
        return Ok(BTreeMap::new());
    }
    meridian_module::Manifest::load(dir)
        .map(|manifest| manifest.settings)
        .map_err(module_failure)
}

pub(crate) fn registry(plugins: &PluginDirs) -> Result<Registry, Failure> {
    let dirs = plugin_dirs(plugins);
    let options = StartOptions {
        verification: plugins.verification()?,
        settings: project_settings()?,
        ..StartOptions::default()
    };
    let mut registry = Registry::new();
    for dir in dirs {
        let loaded = registry
            .discover(&dir.to_string_lossy(), &options)
            .map_err(|e| Failure::Message(e.to_string()))?;
        // Under `warn` a plugin that did not pass was started all the same;
        // this is the saying so.
        for info in loaded {
            match &info.signature {
                Signature::Unsigned => {
                    eprintln!("warning: plugin {} is not signed", info.path)
                }
                Signature::Invalid(why) => eprintln!(
                    "warning: plugin {}: its signature does not pass: {why}",
                    info.path
                ),
                Signature::NotChecked | Signature::Verified { .. } => {}
            }
        }
    }
    Ok(registry)
}

/// The directory the `meridian` binary really is in.
///
/// A package manager often puts a link to the binary on the `PATH` and keeps
/// the binary itself, with its plugins beside it, somewhere else. The link is
/// followed, so the plugins are found wherever the command was started from.
pub(crate) fn own_directory() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe = meridian_sdk::tool::absolute_path(&exe);
    exe.parent().map(Path::to_path_buf)
}

pub(crate) fn import_configuration(
    plugins: &PluginDirs,
    request: &import::Request,
) -> Result<(), Failure> {
    let registry = registry(plugins)?;
    let outcome = import::run(&registry, request);
    if let Err(e) = registry.close() {
        eprintln!("warning: {e}");
    }
    outcome
}

pub(crate) fn list_plugins(plugins: &PluginDirs, with_settings: bool) -> Result<(), Failure> {
    let registry = registry(plugins)?;
    let names = registry.names();
    if names.is_empty() && !json_output() {
        outln!("no plugins found");
        return Ok(());
    }
    let mut listed = Vec::new();
    for name in names {
        let info = registry
            .target(name)
            .expect("a listed target exists")
            .info();
        let mut can = vec!["compile"];
        if info.operations.plan {
            can.push("plan");
        }
        if info.operations.apply {
            can.push("apply");
        }
        if info.operations.outputs {
            can.push("outputs");
        }
        if info.accepts_inputs {
            can.push("inputs");
        }
        if !info.import_formats.is_empty() {
            can.push("import");
        }
        if info.exports_schemas {
            can.push("schemas");
        }
        if json_output() {
            listed.push(output::plugin(name, info, &can));
            continue;
        }
        outln!("{name}");
        outln!("  category:  {}", info.category);
        outln!("  can:       {}", can.join(", "));
        outln!("  resources: {}", info.resource_types.join(", "));
        if !info.target_versions.is_empty() {
            outln!("  versions:  {}", info.target_versions.join(", "));
        }
        for format in &info.import_formats {
            outln!(
                "  imports:   {} ({})",
                format.description,
                format.patterns.join(", ")
            );
        }
        outln!(
            "  sdk:       {} (protocol {})",
            info.sdk_version,
            info.protocol_version
        );
        outln!("  binary:    {}", info.path);
        outln!("  signature: {}", info.signature);
        if with_settings {
            if info.settings.is_empty() {
                outln!("  settings:  none declared");
            } else {
                outln!("  settings:");
                for setting in &info.settings {
                    outln!(
                        "    {} ({}, default {})",
                        setting.name,
                        setting.variable,
                        if setting.default.is_empty() {
                            "none".to_owned()
                        } else {
                            format!("{:?}", setting.default)
                        }
                    );
                    outln!("      {}", setting.description);
                }
            }
        }
    }
    if json_output() {
        emit(true, body([("plugins", listed.into())]));
    }
    registry
        .close()
        .map_err(|e| Failure::Message(e.to_string()))
}
