//! Shared upstream command preparation for the relay and transparent fallback.
//! Resolve Windows shims against the same effective environment the child gets.
use std::collections::BTreeMap;
use std::process::Command;

pub(crate) fn command_with_env(program: &str, args: &[String], environment: &BTreeMap<String, String>) -> Result<Command, String> {
    if program.trim().is_empty() { return Err("Empty MCP command".into()); }
    #[cfg(windows)]
    let mut command = windows_command(program, args, environment)?;
    #[cfg(not(windows))]
    let mut command = {
        let mut command = Command::new(program);
        command.args(args);
        command
    };
    command.envs(environment);
    Ok(command)
}

#[cfg(windows)]
fn effective_path(environment: &BTreeMap<String, String>) -> Option<std::ffi::OsString> {
    // Command::envs applies entries in iteration order. Windows keys compare
    // case-insensitively, so the last PATH/Path entry is the effective override.
    environment.iter().rev().find(|(key, _)| key.eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| std::ffi::OsString::from(value))
        .or_else(|| std::env::var_os("PATH"))
}

#[cfg(windows)]
fn resolve_windows_program(program: &str, paths: Option<&std::ffi::OsStr>) -> std::path::PathBuf {
    use std::path::PathBuf;
    let executable = PathBuf::from(program);
    if executable.extension().is_none() {
        let names = [format!("{program}.exe"), format!("{program}.cmd"), format!("{program}.bat")];
        if executable.components().count() > 1 {
            if let Some(found) = names.iter().map(PathBuf::from).find(|path| path.is_file()) { return found; }
        } else if let Some(paths) = paths {
            for directory in std::env::split_paths(paths) {
                for name in &names {
                    let candidate = directory.join(name);
                    if candidate.is_file() { return candidate; }
                }
            }
        }
    } else if executable.components().count() == 1 && !executable.is_file() {
        if let Some(paths) = paths {
            if let Some(found) = std::env::split_paths(paths)
                .map(|directory| directory.join(program)).find(|candidate| candidate.is_file()) {
                return found;
            }
        }
    }
    executable
}

#[cfg(windows)]
fn windows_command(program: &str, args: &[String], environment: &BTreeMap<String, String>) -> Result<Command, String> {
    use std::os::windows::process::CommandExt;
    use std::path::Path;
    let executable = resolve_windows_program(program, effective_path(environment).as_deref());
    let batch = executable.extension().and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat"));
    if !batch {
        let mut command = Command::new(executable);
        command.args(args);
        return Ok(command);
    }
    // Preserve the existing shell-syntax restrictions for batch shims.
    fn quoted(value: &str) -> Result<String, String> {
        if value.chars().any(|character| matches!(character, '"' | '&' | '|' | '<' | '>' | '^' | '%' | '!' | '\r' | '\n' | '\0')) {
            return Err("Unsafe MCP batch command or argument".into());
        }
        Ok(format!("\"{value}\""))
    }
    let mut line = format!("\"{}", quoted(&executable.to_string_lossy())?);
    for argument in args { line.push(' '); line.push_str(&quoted(argument)?); }
    line.push('"');
    let root = std::env::var_os("SystemRoot").ok_or("Windows system root unavailable")?;
    let processor = Path::new(&root).join("System32").join("cmd.exe");
    if !processor.is_file() { return Err("Windows command processor unavailable".into()); }
    let mut command = Command::new(processor);
    command.args(["/d", "/s", "/c"]);
    command.raw_arg(line);
    Ok(command)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn configured_path_selects_the_requested_tool_and_batch_shim() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("version-a");
        let second = directory.path().join("version-b");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(first.join("fixture.cmd"), b"fixture, never executed").unwrap();
        std::fs::write(second.join("fixture.cmd"), b"fixture, never executed").unwrap();
        let environment = BTreeMap::from([("Path".into(), second.to_string_lossy().into_owned())]);
        assert_eq!(resolve_windows_program("fixture", effective_path(&environment).as_deref()), second.join("fixture.cmd"));
        let paths = std::env::join_paths([&second, &first]).unwrap();
        assert_eq!(resolve_windows_program("fixture.cmd", Some(&paths)), second.join("fixture.cmd"));
    }

    #[test]
    fn windows_path_case_and_empty_override_match_child_environment_semantics() {
        let environment = BTreeMap::from([("PATH".into(), "earlier".into()), ("Path".into(), "later".into())]);
        assert_eq!(effective_path(&environment), Some("later".into()));
        assert_eq!(effective_path(&BTreeMap::from([("PATH".into(), String::new())])), Some("".into()));
    }
}
