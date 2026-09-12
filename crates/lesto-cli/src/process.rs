//! Running the application binary, handing it the listening socket on Unix.

use std::io;
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

pub struct Spawn<'a> {
    pub exe: &'a Path,
    pub args: &'a [String],
    pub host: &'a str,
    pub port: u16,
    /// The socket the child should serve on (fd 3, `LISTEN_FDS=1`). `None` on Windows.
    pub listener: Option<&'a TcpListener>,
}

pub fn spawn(s: &Spawn<'_>) -> io::Result<Child> {
    let mut cmd = Command::new(s.exe);
    cmd.args(s.args)
        .env("LESTO_HOST", s.host)
        .env("LESTO_PORT", s.port.to_string())
        .env("PORT", s.port.to_string())
        .env_remove("LISTEN_FDS")
        .env_remove("LISTEN_PID");
    #[cfg(unix)]
    if let Some(listener) = s.listener {
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;

        let fd = listener.as_raw_fd();
        cmd.env("LISTEN_FDS", "1");
        // SAFETY: only async-signal-safe calls (dup2 / fcntl) between fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                if fd == 3 {
                    // Already the right number: just let it survive exec.
                    if libc::fcntl(3, libc::F_SETFD, 0) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                } else if libc::dup2(fd, 3) == -1 {
                    // dup2 clears FD_CLOEXEC on the new descriptor.
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    cmd.spawn()
}

/// Ask the child to stop (SIGTERM), wait up to `grace`, then kill it.
pub fn stop(child: &mut Child, grace: Duration) -> io::Result<ExitStatus> {
    #[cfg(unix)]
    {
        // SAFETY: plain kill(2) on a pid we own.
        unsafe {
            libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
        }
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    #[cfg(not(unix))]
    let _ = grace;
    child.kill()?;
    child.wait()
}
