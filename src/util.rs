use std::path::Path;
use std::process::Command;

pub fn run_in(dir: &Path, cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .current_dir(dir)
        .output()
        .map(|o| {
            let out = String::from_utf8_lossy(&o.stdout).into_owned();
            let err = String::from_utf8_lossy(&o.stderr).into_owned();
            if out.is_empty() && !err.is_empty() { err } else { out }
        })
        .unwrap_or_else(|e| format!("error: {e}"))
}

pub fn branch_name_for(path: &Path) -> String {
    Command::new("git")
        .args(["symbolic-ref", "--short", "HEAD"])
        .current_dir(path)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "main".to_string())
        })
}
