//! Private same-executable worker dispatch, before any async runtime or config.
use std::ffi::OsStr;

pub const WORKER_ARGUMENT: &str = "--internal-nvenc-worker";

pub fn dispatch_worker() -> Option<i32> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let marked = std::env::var_os("HYPR_RDP_NVENC_WORKER");
    if !arguments.iter().any(|arg| arg == WORKER_ARGUMENT) && marked.is_none() {
        return None;
    }
    if arguments.len() != 1
        || arguments[0] != WORKER_ARGUMENT
        || marked.as_deref() != Some(OsStr::new("1"))
    {
        return Some(2);
    }
    #[cfg(feature = "nvenc")]
    {
        if !private_socket() {
            return Some(2);
        }
        use std::os::fd::FromRawFd;
        // SAFETY: fd3 was validated as this parent's connected Unix socket.
        // Dispatch owns it exclusively and the ordinary server never starts.
        let socket = unsafe { std::os::unix::net::UnixStream::from_raw_fd(super::WORKER_FD) };
        Some(if super::native::serve(socket).is_ok() {
            0
        } else {
            1
        })
    }
    #[cfg(not(feature = "nvenc"))]
    Some(2)
}

#[cfg(feature = "nvenc")]
fn private_socket() -> bool {
    use std::mem::size_of;
    let mut domain: libc::c_int = 0;
    let mut kind: libc::c_int = 0;
    let mut peer = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut domain_len = size_of::<libc::c_int>() as libc::socklen_t;
    let mut kind_len = domain_len;
    let mut peer_len = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: writable socket-option buffers with exact declared lengths.
    unsafe {
        libc::getsockopt(
            super::WORKER_FD,
            libc::SOL_SOCKET,
            libc::SO_DOMAIN,
            (&mut domain as *mut libc::c_int).cast(),
            &mut domain_len,
        ) == 0
            && domain == libc::AF_UNIX
            && libc::getsockopt(
                super::WORKER_FD,
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&mut kind as *mut libc::c_int).cast(),
                &mut kind_len,
            ) == 0
            && kind == libc::SOCK_STREAM
            && libc::getsockopt(
                super::WORKER_FD,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut peer as *mut libc::ucred).cast(),
                &mut peer_len,
            ) == 0
            && peer.uid == libc::geteuid()
            && peer.pid == libc::getppid()
    }
}
