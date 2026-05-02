use std::path::PathBuf;

#[derive(Clone)]
pub struct Worktree {
    pub path: PathBuf,
    pub name: String,
}

#[derive(Clone)]
pub struct Branch {
    pub name: String,
    pub is_current: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SbsKind {
    Context,
    Removed,
    Added,
    Changed,
    Header,
}

#[derive(Clone)]
pub struct SbsRow {
    pub left_no: Option<usize>,
    pub left: String,
    pub right_no: Option<usize>,
    pub right: String,
    pub kind: SbsKind,
}

pub struct FileDiff {
    pub filename: String,
    pub content: String,
    pub sbs: Vec<SbsRow>,
}
