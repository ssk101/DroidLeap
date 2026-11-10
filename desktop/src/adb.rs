//! The adb server keeps the Android device's ADB USB interface to itself,
//! which DroidLeap needs, so we can find it and ask it to exit.
//!
//! This speaks adb's "smart socket" protocol directly, so no adb binary is needed.

use std::time::Duration;

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::{Instant, sleep, timeout},
};

const DEFAULT_PORT: u16 = 5037;
const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);
const KILL_TIMEOUT: Duration = Duration::from_secs(3);
/// 4 hex digits of length, then the request.
const KILL_REQUEST: &[u8] = b"0009host:kill";

fn server_port() -> u16 {
    std::env::var("ANDROID_ADB_SERVER_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

pub async fn is_running() -> bool {
    running_at(server_port()).await
}

/// Asks the adb server to exit and waits until it has.
pub async fn kill() -> Result<(), String> {
    kill_at(server_port()).await
}

async fn connect(port: u16) -> Option<TcpStream> {
    timeout(CONNECT_TIMEOUT, TcpStream::connect(("127.0.0.1", port)))
        .await
        .ok()?
        .ok()
}

async fn running_at(port: u16) -> bool {
    connect(port).await.is_some()
}

async fn kill_at(port: u16) -> Result<(), String> {
    let Some(mut stream) = connect(port).await else {
        return Ok(()); // already gone
    };
    stream
        .write_all(KILL_REQUEST)
        .await
        .map_err(|e| format!("cannot reach the adb server: {e}"))?;
    let mut reply = [0u8; 4];
    stream
        .read_exact(&mut reply)
        .await
        .map_err(|e| format!("the adb server did not answer: {e}"))?;
    if &reply != b"OKAY" {
        return Err(format!(
            "the adb server refused to stop: {}",
            String::from_utf8_lossy(&reply)
        ));
    }
    drop(stream);

    let deadline = Instant::now() + KILL_TIMEOUT;
    while running_at(port).await {
        if Instant::now() >= deadline {
            return Err("the adb server is still running".into());
        }
        sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tokio::net::TcpListener;

    use super::*;

    /// A stand-in adb server. Connections that send nothing are probes and are ignored.
    /// A real request gets `reply`, and with `exits` the server then stops listening.
    async fn fake_server(reply: &'static [u8], exits: bool) -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = vec![0u8; KILL_REQUEST.len()];
                if stream.read_exact(&mut request).await.is_err() {
                    continue;
                }
                assert_eq!(request, KILL_REQUEST);
                stream.write_all(reply).await.unwrap();
                if exits {
                    return;
                }
            }
        });
        port
    }

    #[tokio::test]
    async fn nothing_listening_means_not_running_and_nothing_to_kill() {
        let port = {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            listener.local_addr().unwrap().port()
        };
        assert!(!running_at(port).await);
        assert_eq!(kill_at(port).await, Ok(()));
    }

    #[tokio::test]
    async fn sends_host_kill_and_waits_for_the_server_to_go() {
        let port = fake_server(b"OKAY", true).await;
        assert!(running_at(port).await);
        assert_eq!(kill_at(port).await, Ok(()));
        assert!(!running_at(port).await);
    }

    #[tokio::test]
    async fn a_refusal_is_an_error() {
        let port = fake_server(b"FAIL", false).await;
        let error = kill_at(port).await.unwrap_err();
        assert!(error.contains("FAIL"), "{error}");
    }
}
