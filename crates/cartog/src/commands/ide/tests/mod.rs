//! Tests for the ide command modules, split by concern.

pub(super) fn args() -> Vec<String> {
    vec!["serve".into()]
}

pub(super) fn args_watch() -> Vec<String> {
    vec!["serve".into(), "--watch".into()]
}

mod catalogue;
mod merge;
mod picker;
mod run;
