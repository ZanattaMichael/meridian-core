//! One module per group of subcommands. Each takes what the arguments in
//! `args` parsed and returns a [`Failure`](super::Failure) or nothing.

pub(crate) mod documents;
pub(crate) mod logs;
pub(crate) mod modules;
pub(crate) mod plugins;
pub(crate) mod retarget;
pub(crate) mod rules;
pub(crate) mod run;
pub(crate) mod secrets;
pub(crate) mod signing;
pub(crate) mod testing;

pub(crate) use documents::*;
pub(crate) use logs::*;
pub(crate) use modules::*;
pub(crate) use plugins::*;
pub(crate) use retarget::*;
pub(crate) use rules::*;
pub(crate) use run::*;
pub(crate) use secrets::*;
pub(crate) use signing::*;
pub(crate) use testing::*;
