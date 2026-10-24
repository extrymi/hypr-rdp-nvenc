//! Real executable dispatch without sending Init: no GPU libraries are loaded.
#![cfg(target_os = "linux")]

#[cfg(feature = "nvenc")]
use std::process::Stdio;
use std::time::Duration;

#[tokio::test]
async fn private_worker_rejects_missing_marker_socket_and_extra_arguments() {
    for (args, marker) in [
        (vec!["--internal-nvenc-worker"], None),
        (vec!["--internal-nvenc-worker"], Some("1")),
        (vec!["--internal-nvenc-worker", "--help"], Some("1")),
        (vec!["--help"], Some("1")),
    ] {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_hypr-rdp"));
        command.env_clear().args(args).kill_on_drop(true);
        if let Some(marker) = marker {
            command.env("HYPR_RDP_NVENC_WORKER", marker);
        }
        let output = tokio::time::timeout(Duration::from_secs(3), command.output())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
}

#[cfg(feature = "nvenc")]
#[tokio::test]
async fn actual_worker_sends_hello_and_exits_without_init_or_desktop() {
    use std::os::fd::AsRawFd;
    use tokio::io::AsyncReadExt;
    let (parent, child_socket) = std::os::unix::net::UnixStream::pair().unwrap();
    parent.set_nonblocking(true).unwrap();
    let descriptor = child_socket.as_raw_fd();
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_hypr-rdp"));
    command
        .env_clear()
        .env("HYPR_RDP_NVENC_WORKER", "1")
        .arg("--internal-nvenc-worker")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    // SAFETY: same dup2/CLOEXEC contract as the production supervisor.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(descriptor, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    drop(child_socket);
    let mut stream = tokio::net::UnixStream::from_std(parent).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        assert_eq!(stream.read_u32_le().await.unwrap(), 5);
        let mut hello = [0; 5];
        stream.read_exact(&mut hello).await.unwrap();
        assert_eq!(&hello, b"HNV1\0");
    })
    .await
    .unwrap();
    drop(stream);
    let status = tokio::time::timeout(Duration::from_secs(3), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.code(), Some(1));
}
