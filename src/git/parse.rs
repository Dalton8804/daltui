use std::path::PathBuf;

use super::types::{Branch, FileDiff, SbsKind, SbsRow, Worktree};

pub fn parse_worktrees(raw: &str) -> Vec<Worktree> {
    raw.split("\n\n")
        .filter(|s| !s.trim().is_empty())
        .filter_map(|block| {
            let mut path: Option<PathBuf> = None;
            let mut name: Option<String> = None;
            let mut detached = false;
            for line in block.lines() {
                if let Some(p) = line.strip_prefix("worktree ") {
                    path = Some(PathBuf::from(p.trim()));
                } else if let Some(b) = line.strip_prefix("branch ") {
                    name = Some(b.trim().trim_start_matches("refs/heads/").to_string());
                } else if line.trim() == "detached" || line.trim() == "bare" {
                    detached = true;
                }
            }
            let path = path?;
            let name = name.unwrap_or_else(|| {
                if detached {
                    "(detached)".to_string()
                } else {
                    path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "?".to_string())
                }
            });
            Some(Worktree { path, name })
        })
        .collect()
}

pub fn parse_branches(raw: &str) -> Vec<Branch> {
    raw.lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty() && !t.contains("remotes/")
        })
        .filter_map(|line| {
            let is_current = line.starts_with("* ");
            let rest = line.strip_prefix("* ").or_else(|| line.strip_prefix("  "))?;
            let name = rest.split_whitespace().next()?.to_string();
            if name.is_empty() {
                return None;
            }
            Some(Branch { name, is_current })
        })
        .collect()
}

pub fn parse_diff(raw: &str) -> Vec<FileDiff> {
    let mut result = Vec::new();
    let (mut content, mut filename) = (String::new(), String::new());
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if !filename.is_empty() {
                result.push(FileDiff {
                    filename: filename.clone(),
                    sbs: parse_side_by_side(&content),
                    content: content.clone(),
                });
            }
            filename = rest
                .split_whitespace()
                .nth(1)
                .unwrap_or(rest)
                .trim_start_matches("b/")
                .to_string();
            content = line.to_string();
        } else {
            content.push('\n');
            content.push_str(line);
        }
    }
    if !filename.is_empty() {
        result.push(FileDiff {
            filename,
            sbs: parse_side_by_side(&content),
            content,
        });
    }
    result
}

fn parse_hunk_start(line: &str) -> Option<(usize, usize)> {
    let s = line.strip_prefix("@@ ")?;
    let mut parts = s.split_whitespace();
    let old: usize = parts.next()?.strip_prefix('-')?.split(',').next()?.parse().ok()?;
    let new: usize = parts.next()?.strip_prefix('+')?.split(',').next()?.parse().ok()?;
    Some((old, new))
}

fn flush_sbs(
    rows: &mut Vec<SbsRow>,
    rem: &mut Vec<(usize, String)>,
    add: &mut Vec<(usize, String)>,
) {
    for i in 0..rem.len().max(add.len()) {
        let kind = match (i < rem.len(), i < add.len()) {
            (true, true) => SbsKind::Changed,
            (true, false) => SbsKind::Removed,
            _ => SbsKind::Added,
        };
        let (l_no, l) = rem.get(i).cloned().unwrap_or_default();
        let (r_no, r) = add.get(i).cloned().unwrap_or_default();
        rows.push(SbsRow {
            left_no: if i < rem.len() { Some(l_no) } else { None },
            left: l,
            right_no: if i < add.len() { Some(r_no) } else { None },
            right: r,
            kind,
        });
    }
    rem.clear();
    add.clear();
}

fn parse_side_by_side(content: &str) -> Vec<SbsRow> {
    let mut rows = Vec::new();
    let (mut ln, mut rn) = (1usize, 1usize);
    let (mut rem, mut add) = (Vec::new(), Vec::new());
    for line in content.lines() {
        if line.starts_with("@@") {
            flush_sbs(&mut rows, &mut rem, &mut add);
            if let Some((l, r)) = parse_hunk_start(line) {
                ln = l;
                rn = r;
            }
            rows.push(SbsRow {
                left_no: None,
                left: line.to_string(),
                right_no: None,
                right: String::new(),
                kind: SbsKind::Header,
            });
        } else if line.starts_with("diff ")
            || line.starts_with("index ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
        {
            flush_sbs(&mut rows, &mut rem, &mut add);
            rows.push(SbsRow {
                left_no: None,
                left: line.to_string(),
                right_no: None,
                right: String::new(),
                kind: SbsKind::Header,
            });
        } else if let Some(rest) = line.strip_prefix('-') {
            rem.push((ln, rest.to_string()));
            ln += 1;
        } else if let Some(rest) = line.strip_prefix('+') {
            add.push((rn, rest.to_string()));
            rn += 1;
        } else {
            flush_sbs(&mut rows, &mut rem, &mut add);
            let text = line.strip_prefix(' ').unwrap_or(line);
            rows.push(SbsRow {
                left_no: Some(ln),
                left: text.to_string(),
                right_no: Some(rn),
                right: text.to_string(),
                kind: SbsKind::Context,
            });
            ln += 1;
            rn += 1;
        }
    }
    flush_sbs(&mut rows, &mut rem, &mut add);
    rows
}
