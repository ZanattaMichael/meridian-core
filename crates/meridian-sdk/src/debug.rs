//! A tool's own debug output, at the level `meridian --tool-debug` asks for.
//!
//! A runner declares what each level adds to its tool, in one table, and
//! applies the level the run asks for when it builds the tool:
//!
//! ```
//! use meridian_sdk::debug::{self, DebugLevel};
//! use meridian_sdk::tool::Tool;
//!
//! const DEBUG: &[DebugLevel] = &[
//!     DebugLevel::args(&["-v"]),
//!     DebugLevel::args(&["-vvv"]),
//!     DebugLevel::args(&["-vvvv"]),
//! ];
//!
//! let tool = Tool::new("ansible-playbook").debug(debug::pick(DEBUG, 2));
//! assert_eq!(tool.command_line(), "ansible-playbook -vvv");
//! ```
//!
//! The same table is what [`Runner::debug_levels`](crate::Runner::debug_levels)
//! returns, so what `meridian plugins` lists is what the runner does.

/// The highest level a run asks for.
pub const MAX_LEVEL: u32 = 3;

/// What one debug level adds to the tool: arguments, environment, and what
/// the runner does itself, in words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DebugLevel {
    pub args: &'static [&'static str],
    pub env: &'static [(&'static str, &'static str)],
    /// What the runner does itself at this level, such as keeping more of a
    /// journal. Not given to the tool.
    pub does: &'static str,
}

impl DebugLevel {
    /// A level that adds arguments.
    pub const fn args(args: &'static [&'static str]) -> DebugLevel {
        DebugLevel {
            args,
            env: &[],
            does: "",
        }
    }

    /// A level that sets environment variables.
    pub const fn env(env: &'static [(&'static str, &'static str)]) -> DebugLevel {
        DebugLevel {
            args: &[],
            env,
            does: "",
        }
    }

    /// A level the runner applies itself, described in words.
    pub const fn does(does: &'static str) -> DebugLevel {
        DebugLevel {
            args: &[],
            env: &[],
            does,
        }
    }

    /// The level as a person reads it: `TF_LOG=DEBUG`, `-vvv`.
    pub fn describe(&self) -> String {
        let mut words: Vec<String> = self.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        words.extend(self.args.iter().map(|a| (*a).to_owned()));
        if !self.does.is_empty() {
            words.push(self.does.to_owned());
        }
        words.join(" ")
    }
}

/// Each level as a person reads it, level 1 first: the runner's main table,
/// then each other tool's, named. A table shorter than another repeats its
/// last level. This is what `meridian plugins` lists.
pub fn describe_all(
    main: &'static [DebugLevel],
    more: &[(&'static str, &'static [DebugLevel])],
) -> Vec<String> {
    let count = more
        .iter()
        .map(|(_, table)| table.len())
        .chain([main.len()])
        .max()
        .unwrap_or(0);
    (1..=count as u32)
        .map(|level| {
            let mut parts: Vec<String> = pick(main, level)
                .map(DebugLevel::describe)
                .into_iter()
                .collect();
            for (tool, table) in more {
                if let Some(picked) = pick(table, level) {
                    parts.push(format!("{tool}: {}", picked.describe()));
                }
            }
            parts.join("; ")
        })
        .collect()
}

/// The level of `levels` to use for a run that asks for `level`: none for
/// 0, or when the runner declares none; the highest declared for a level
/// above it.
pub fn pick(levels: &'static [DebugLevel], level: u32) -> Option<&'static DebugLevel> {
    if level == 0 || levels.is_empty() {
        return None;
    }
    let index = (level.min(MAX_LEVEL) as usize).min(levels.len()) - 1;
    levels.get(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    const THREE: &[DebugLevel] = &[
        DebugLevel::env(&[("TF_LOG", "INFO")]),
        DebugLevel::env(&[("TF_LOG", "DEBUG")]),
        DebugLevel::env(&[("TF_LOG", "TRACE")]),
    ];
    const ONE: &[DebugLevel] = &[DebugLevel::env(&[("PACKER_LOG", "1")])];

    #[test]
    fn a_level_picks_its_entry_and_a_higher_one_the_last() {
        assert_eq!(pick(THREE, 0), None);
        assert_eq!(pick(THREE, 2).unwrap().describe(), "TF_LOG=DEBUG");
        assert_eq!(pick(THREE, 9).unwrap().describe(), "TF_LOG=TRACE");
        assert_eq!(pick(ONE, 3).unwrap().describe(), "PACKER_LOG=1");
        assert_eq!(pick(&[], 1), None);
    }

    #[test]
    fn a_level_reads_as_its_environment_then_its_arguments() {
        let level = DebugLevel {
            args: &["-v=6", "--logtostderr"],
            env: &[("A", "1")],
            does: "",
        };
        assert_eq!(level.describe(), "A=1 -v=6 --logtostderr");
    }

    #[test]
    fn every_table_is_described_at_each_level_the_longest_has() {
        const OTHER: &[DebugLevel] = &[DebugLevel::does("keeps more")];
        assert_eq!(
            describe_all(ONE, &[("other", THREE), ("third", OTHER)]),
            [
                "PACKER_LOG=1; other: TF_LOG=INFO; third: keeps more",
                "PACKER_LOG=1; other: TF_LOG=DEBUG; third: keeps more",
                "PACKER_LOG=1; other: TF_LOG=TRACE; third: keeps more",
            ]
        );
        assert!(describe_all(&[], &[]).is_empty());
    }
}
