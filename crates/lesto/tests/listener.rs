//! `lesto::listener` picks up a socket inherited under the `LISTEN_FDS` protocol.

#[cfg(unix)]
#[test]
fn inherited_socket_on_fd_3_is_used() {
    use std::os::fd::AsRawFd;

    // A socket bound by "the parent", duplicated onto fd 3 as `lesto dev` would do. This
    // must happen before the tokio runtime exists, or fd 3 would be its kqueue/epoll handle.
    let parent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = parent.local_addr().unwrap();
    if parent.as_raw_fd() == 3 {
        // Already fd 3 (the first free descriptor): hand ownership over to the inheriting side.
        std::mem::forget(parent);
    } else {
        // SAFETY: dup2 onto fd 3, which this test process does not use otherwise.
        assert!(unsafe { libc::dup2(parent.as_raw_fd(), 3) } == 3);
    }
    // SAFETY: no other thread exists yet (the runtime is built below).
    unsafe {
        std::env::set_var("LISTEN_FDS", "1");
        std::env::set_var("LISTEN_PID", std::process::id().to_string());
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        // Port 1 could not be bound: the inherited socket must be used instead.
        let listener = lesto::listener("127.0.0.1:1").await.unwrap();
        assert_eq!(listener.local_addr().unwrap(), addr);
        // The environment is left alone (mutating it at runtime is unsound), but the socket
        // is taken at most once: a second call binds normally.
        assert_eq!(std::env::var("LISTEN_FDS").as_deref(), Ok("1"));
        let plain = lesto::listener("127.0.0.1:0").await.unwrap();
        assert_ne!(plain.local_addr().unwrap(), addr);
    });
}
