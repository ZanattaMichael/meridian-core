//! How a runner is configured: the settings it declares, and the one way
//! they are read.
//!
//! A runner needs an operator's choices that no document should carry: where
//! the tool is installed, which profile or context to use, extra arguments
//! for every call. Each such choice is a [`Setting`]: a short name, the
//! environment variable it is read from, its default and what it does. A
//! runner lists its settings from [`Runner::settings`](crate::Runner::settings),
//! so `meridian plugins --settings` can print them and the reference page
//! can be generated from them, and reads each one through [`text`],
//! [`optional`], [`args`] or [`switch`] here, so every plugin reads them the
//! same way.
//!
//! A setting's value comes from the first of these that has it:
//!
//! 1. The environment variable, when it is set and not blank. It always
//!    wins, so a shell or a CI job can override a project's choice.
//! 2. The project's `meridian.yaml`, under `spec.settings.<target>.<name>`.
//!    The host hands the target's map to the plugin in
//!    [`PROJECT_SETTINGS_ENV`] when it starts it.
//! 3. The setting's default.
//!
//! An argument list is split as a POSIX shell splits a command line, so an
//! argument that holds a space can be written in quotes:
//! `--extra-vars "a=b c"`.

use std::collections::BTreeMap;

/// The environment variable the host sets on a plugin it starts, holding the
/// project's settings for that target as a JSON object of setting name to
/// value.
pub const PROJECT_SETTINGS_ENV: &str = "MERIDIAN_SETTINGS";

/// One thing an operator can configure about a runner.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Setting {
    /// A short name, lower-case words joined by `-`, such as `program` or
    /// `args`. It is the key under `spec.settings.<target>` in
    /// `meridian.yaml`, and unique within the plugin.
    pub name: String,
    /// The environment variable the setting is read from, such as
    /// `MERIDIAN_ANSIBLE_ARGS`. It starts with `MERIDIAN_`.
    pub variable: String,
    /// The value when nothing sets it; empty when there is none.
    pub default: String,
    /// One or two sentences on what the setting does and what values it
    /// takes, for a listing.
    pub description: String,
}

impl Setting {
    pub fn new(name: &str, variable: &str, default: &str, description: &str) -> Setting {
        Setting {
            name: name.to_owned(),
            variable: variable.to_owned(),
            default: default.to_owned(),
            description: description.to_owned(),
        }
    }

    /// Whether `name` has the shape a setting name must have: one or more
    /// lower-case words of letters and digits, joined by `-`.
    pub fn valid_name(name: &str) -> bool {
        !name.is_empty()
            && !name.starts_with('-')
            && !name.ends_with('-')
            && !name.contains("--")
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    }
}

/// The setting called `name` among those a runner declares. A runner reads
/// only settings it declares, so a name that is not there is a mistake in
/// the plugin, and this panics with its name.
pub fn find<'a>(declared: &'a [Setting], name: &str) -> &'a Setting {
    declared
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("the runner reads setting {name:?} but does not declare it"))
}

/// The project's settings for this plugin, as the host handed them over. A
/// value the host could not send as text is left out.
pub fn project() -> BTreeMap<String, String> {
    let Ok(text) = std::env::var(PROJECT_SETTINGS_ENV) else {
        return BTreeMap::new();
    };
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(&text)
    else {
        return BTreeMap::new();
    };
    map.into_iter()
        .filter_map(|(name, value)| match value {
            serde_json::Value::String(s) => Some((name, s)),
            serde_json::Value::Bool(b) => Some((name, b.to_string())),
            serde_json::Value::Number(n) => Some((name, n.to_string())),
            _ => None,
        })
        .collect()
}

/// Encodes a target's settings for [`PROJECT_SETTINGS_ENV`], the way
/// [`project`] reads them back.
pub fn encode_project(settings: &BTreeMap<String, String>) -> String {
    serde_json::Value::Object(
        settings
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect(),
    )
    .to_string()
}

/// The value of an environment variable, when it is set and not blank.
pub fn variable(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// The setting's value, from the environment, the project or its default,
/// in that order. See the [module documentation](self).
pub fn text(setting: &Setting) -> String {
    optional(setting).unwrap_or_default()
}

/// Like [`text`], but `None` when nothing sets the value and the setting has
/// no default.
pub fn optional(setting: &Setting) -> Option<String> {
    variable(&setting.variable)
        .or_else(|| {
            project()
                .remove(&setting.name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        })
        .or_else(|| {
            if setting.default.is_empty() {
                None
            } else {
                Some(setting.default.clone())
            }
        })
}

/// The setting's value as a list of arguments, split like a shell command
/// line. See [`words`].
pub fn args(setting: &Setting) -> Vec<String> {
    words(&text(setting))
}

/// The setting's value as yes or no: `1`, `true`, `yes` and `on` are true,
/// `0`, `false`, `no` and `off` are false, whatever the case. Anything else
/// is `None`, for the runner to report as a misconfiguration.
pub fn switch(setting: &Setting) -> Option<bool> {
    match text(setting).to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Splits text into words as a POSIX shell splits a command line: words are
/// separated by blanks, `"..."` and `'...'` hold a word together, and a
/// backslash outside single quotes takes the next character as it is. A
/// quote that is never closed takes the rest of the text, and a backslash
/// at the end is dropped.
pub fn words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    word.push(c);
                }
            }
            '"' => {
                in_word = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some(e @ ('"' | '\\' | '$' | '`')) => word.push(e),
                            Some(other) => {
                                word.push('\\');
                                word.push(other);
                            }
                            None => word.push('\\'),
                        },
                        other => word.push(other),
                    }
                }
            }
            '\\' => {
                if let Some(next) = chars.next() {
                    in_word = true;
                    word.push(next);
                }
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            other => {
                in_word = true;
                word.push(other);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The environment is shared by every test in the process.
    static ENV: Mutex<()> = Mutex::new(());

    fn with_env<T>(vars: &[(&str, Option<&str>)], f: impl FnOnce() -> T) -> T {
        let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let before: Vec<(String, Option<String>)> = vars
            .iter()
            .map(|(k, _)| (k.to_string(), std::env::var(k).ok()))
            .collect();
        for (k, v) in vars {
            // Tests that touch the environment are serialised by the lock
            // above, and nothing else in this crate's tests reads these
            // variables.
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        let out = f();
        for (k, v) in before {
            match v {
                Some(v) => std::env::set_var(&k, v),
                None => std::env::remove_var(&k),
            }
        }
        out
    }

    fn setting() -> Setting {
        Setting::new("args", "MERIDIAN_SETTINGS_TEST_ARGS", "", "extra arguments")
    }

    #[test]
    fn words_split_like_a_shell() {
        assert_eq!(words(""), Vec::<String>::new());
        assert_eq!(words("   "), Vec::<String>::new());
        assert_eq!(words("--user deploy"), vec!["--user", "deploy"]);
        assert_eq!(
            words(r#"--extra-vars "a=b c" --key 'my key'"#),
            vec!["--extra-vars", "a=b c", "--key", "my key"]
        );
        assert_eq!(
            words(r"C:\Program\ Files\tool"),
            vec!["C:Program Filestool"]
        );
        assert_eq!(
            words(r#""C:\\Program Files\\tool""#),
            vec![r"C:\Program Files\tool"]
        );
        assert_eq!(words(r#""" '' x"#), vec!["", "", "x"]);
        assert_eq!(words(r#"--msg "it's" done"#), vec!["--msg", "it's", "done"]);
        assert_eq!(words(r#""\$HOME" "a\nb""#), vec!["$HOME", r"a\nb"]);
        // An unterminated quote takes the rest; a trailing backslash is
        // dropped rather than becoming a word.
        assert_eq!(words(r#"--a "b c"#), vec!["--a", "b c"]);
        assert_eq!(words(r"a \"), vec!["a"]);
        assert_eq!(words("a\tb\nc"), vec!["a", "b", "c"]);
    }

    #[test]
    fn the_environment_wins_over_the_project_which_wins_over_the_default() {
        let s = Setting::new("args", "MERIDIAN_SETTINGS_TEST_ARGS", "--default", "");
        let project = encode_project(&[("args".to_owned(), "--project 'a b'".to_owned())].into());
        with_env(
            &[
                ("MERIDIAN_SETTINGS_TEST_ARGS", None),
                (PROJECT_SETTINGS_ENV, None),
            ],
            || {
                assert_eq!(text(&s), "--default");
                assert_eq!(args(&s), vec!["--default"]);
            },
        );
        with_env(
            &[
                ("MERIDIAN_SETTINGS_TEST_ARGS", None),
                (PROJECT_SETTINGS_ENV, Some(&project)),
            ],
            || {
                assert_eq!(text(&s), "--project 'a b'");
                assert_eq!(args(&s), vec!["--project", "a b"]);
            },
        );
        with_env(
            &[
                ("MERIDIAN_SETTINGS_TEST_ARGS", Some("  --env  ")),
                (PROJECT_SETTINGS_ENV, Some(&project)),
            ],
            || assert_eq!(text(&s), "--env"),
        );
        // A blank variable is not a value.
        with_env(
            &[
                ("MERIDIAN_SETTINGS_TEST_ARGS", Some("  ")),
                (PROJECT_SETTINGS_ENV, Some(&project)),
            ],
            || assert_eq!(text(&s), "--project 'a b'"),
        );
    }

    #[test]
    fn an_optional_setting_with_nothing_set_is_none() {
        with_env(
            &[
                ("MERIDIAN_SETTINGS_TEST_ARGS", None),
                (PROJECT_SETTINGS_ENV, None),
            ],
            || {
                assert_eq!(optional(&setting()), None);
                assert_eq!(text(&setting()), "");
                assert!(args(&setting()).is_empty());
            },
        );
    }

    #[test]
    fn a_switch_reads_the_usual_words() {
        let s = Setting::new("validate", "MERIDIAN_SETTINGS_TEST_SWITCH", "on", "");
        for (value, want) in [
            (None, Some(true)),
            (Some("off"), Some(false)),
            (Some("TRUE"), Some(true)),
            (Some("0"), Some(false)),
            (Some("maybe"), None),
        ] {
            with_env(
                &[
                    ("MERIDIAN_SETTINGS_TEST_SWITCH", value),
                    (PROJECT_SETTINGS_ENV, None),
                ],
                || assert_eq!(switch(&s), want, "{value:?}"),
            );
        }
    }

    #[test]
    fn project_settings_that_are_not_text_are_read_as_text_or_left_out() {
        with_env(
            &[(
                PROJECT_SETTINGS_ENV,
                Some(r#"{"a":"x","b":true,"c":3,"d":[1],"e":null}"#),
            )],
            || {
                let got = project();
                assert_eq!(got.get("a").map(String::as_str), Some("x"));
                assert_eq!(got.get("b").map(String::as_str), Some("true"));
                assert_eq!(got.get("c").map(String::as_str), Some("3"));
                assert!(!got.contains_key("d"));
                assert!(!got.contains_key("e"));
            },
        );
        with_env(&[(PROJECT_SETTINGS_ENV, Some("not json"))], || {
            assert!(project().is_empty())
        });
    }

    #[test]
    fn setting_names_are_lower_case_words() {
        for ok in ["args", "program", "ssh-args", "output-wait", "v2"] {
            assert!(Setting::valid_name(ok), "{ok}");
        }
        for bad in ["", "Args", "ssh_args", "-args", "args-", "a--b", "a b"] {
            assert!(!Setting::valid_name(bad), "{bad}");
        }
    }
}
