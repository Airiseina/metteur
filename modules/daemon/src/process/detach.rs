//! Detach the daemon from the controlling terminal and run it in the
//! background.
//!
//! `detach` re-executes the current binary with `--detach` stripped from the
//! argument vector, so the child continues normal startup in a new session
//! (Unix) or as a detached process (Windows). The parent returns immediately
//! after recording the child PID, which guarantees the daemon survives the
//! caller (the CLI) and is never its simple child process.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::DaemonResult;

/// Re-executes this binary as a detached background daemon.
///
/// `pid_file` records the detached child's PID. The child inherits the original
/// arguments minus `--detach`, so it proceeds through the normal startup path.
pub fn detach(pid_file: &Path) -> DaemonResult<()> {
    let args: Vec<String> = std::env::args().skip(1).filter(|a| a != "--detach").collect();
    let exe = std::env::current_exe()?;

    #[cfg(windows)]
    let child = {
        use std::os::windows::process::CommandExt;
        // Create the process without inheriting the console.
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        Command::new(&exe)
            .args(&args)
            .creation_flags(DETACHED_PROCESS)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?
    };

    #[cfg(unix)]
    let child = {
        use std::os::unix::process::CommandExt;
        // Create a new session and detach from the controlling terminal.
        unsafe {
            Command::new(&exe)
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .pre_exec(|| {
                    libc::setsid();
                    Ok::<(), std::io::Error>(())
                })
                .spawn()?
        }
    };

    write_pid(pid_file, child.id())?;
    Ok(())
}

/// Writes `pid` to `pid_file`, creating parent directories as needed.
fn write_pid(pid_file: &Path, pid: u32) -> DaemonResult<()> {
    if let Some(parent) = pid_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(pid_file, pid.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_file_is_written_with_pid() {
        let dir = std::env::temp_dir().join(format!("metteur-pid-{}", uuid::Uuid::new_v4()));
        let file = dir.join("nested").join("daemon.pid");
        write_pid(&file, 1234).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "1234");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}