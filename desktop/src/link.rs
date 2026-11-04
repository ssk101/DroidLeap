//! The PC's end of the LAN link to the tablet app: finds the tablet via
//! DNS-SD, connects, says which edge leads back, and forwards the tablet's
//! edge reports. JSON objects, one per line.
//!
//! Not authenticated yet: a peer on the LAN could pose as the tablet. Only
//! edge reports travel over it, so the worst case is an unwanted handback.

use std::{net::SocketAddr, time::Duration};

use mdns_sd::{ServiceDaemon, ServiceEvent};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    sync::mpsc,
    task::JoinHandle,
};

use crate::{geometry::Side, log};

pub const PROTOCOL_VERSION: u32 = 1;
const SERVICE_TYPE: &str = "_tablet-edge._tcp.local.";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const RETRY_DELAY: Duration = Duration::from_secs(3);
/// Longest accepted message line, so a peer can't make us buffer without limit.
const MAX_LINE: usize = 4096;

#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum TabletMessage {
    Hello {
        version: u32,
        app: String,
        width: u32,
        height: u32,
        rotation: u32,
    },
    Display {
        width: u32,
        height: u32,
        rotation: u32,
    },
    Edge {
        edge: Side,
        state: HoverState,
        along: f64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HoverState {
    Enter,
    Move,
    Exit,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum PcMessage {
    Hello { version: u32, edge: Side },
}

#[derive(Debug)]
pub enum LinkEvent {
    Connected(String),
    Message(TabletMessage),
    Disconnected(String),
}

/// A running link. Dropping it stops the link.
pub struct Link {
    task: JoinHandle<()>,
    pub events: mpsc::Receiver<LinkEvent>,
}

impl Drop for Link {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Starts keeping a connection to the tablet app, asking it to watch `tablet_edge`.
pub fn spawn(mdns: ServiceDaemon, tablet_edge: Side) -> Link {
    let (sender, events) = mpsc::channel(64);
    let task = tokio::spawn(run(mdns, tablet_edge, sender));
    Link { task, events }
}

/// A link that never connects, for when mDNS is unavailable.
pub fn disabled() -> Link {
    let (sender, events) = mpsc::channel(1);
    let task = tokio::spawn(async move {
        let _keep_open = sender;
        std::future::pending::<()>().await;
    });
    Link { task, events }
}

async fn run(mdns: ServiceDaemon, tablet_edge: Side, events: mpsc::Sender<LinkEvent>) {
    loop {
        let reason = match session(&mdns, tablet_edge, &events).await {
            Ok(()) => "connection closed".to_string(),
            Err(e) => e,
        };
        if events.send(LinkEvent::Disconnected(reason)).await.is_err() {
            return;
        }
        tokio::time::sleep(RETRY_DELAY).await;
    }
}

async fn session(
    mdns: &ServiceDaemon,
    tablet_edge: Side,
    events: &mpsc::Sender<LinkEvent>,
) -> Result<(), String> {
    let (name, addrs) = discover(mdns).await?;
    let stream = connect(&addrs).await?;
    stream
        .set_nodelay(true)
        .map_err(|e| format!("set_nodelay: {e}"))?;
    let (read, mut write) = stream.into_split();

    let hello = serde_json::to_string(&PcMessage::Hello {
        version: PROTOCOL_VERSION,
        edge: tablet_edge,
    })
    .map_err(|e| format!("encode hello: {e}"))?;
    write
        .write_all(format!("{hello}\n").as_bytes())
        .await
        .map_err(|e| format!("send hello: {e}"))?;
    send(events, LinkEvent::Connected(name)).await?;

    let mut reader = BufReader::new(read);
    let mut line = String::new();
    while read_line(&mut reader, &mut line)
        .await
        .map_err(|e| format!("receive: {e}"))?
    {
        match serde_json::from_str::<TabletMessage>(&line) {
            Ok(message) => send(events, LinkEvent::Message(message)).await?,
            Err(e) => log(format!("link: ignoring message {:?}: {e}", line.trim_end())),
        }
    }
    Ok(())
}

async fn send(events: &mpsc::Sender<LinkEvent>, event: LinkEvent) -> Result<(), String> {
    events
        .send(event)
        .await
        .map_err(|_| "shutting down".to_string())
}

/// Waits until a tablet app is advertised on the LAN. Returns its name and addresses.
async fn discover(mdns: &ServiceDaemon) -> Result<(String, Vec<SocketAddr>), String> {
    let found = mdns
        .browse(SERVICE_TYPE)
        .map_err(|e| format!("mDNS browse: {e}"))?;
    let result = async {
        while let Ok(event) = found.recv_async().await {
            let ServiceEvent::ServiceResolved(service) = event else {
                continue;
            };
            let port = service.get_port();
            let addrs: Vec<SocketAddr> = service
                .get_addresses_v4()
                .into_iter()
                .map(|ip| SocketAddr::from((ip, port)))
                .collect();
            if !addrs.is_empty() {
                let name = service
                    .get_fullname()
                    .trim_end_matches(SERVICE_TYPE)
                    .trim_end_matches('.');
                return Ok((name.to_string(), addrs));
            }
        }
        Err("mDNS browsing stopped".to_string())
    }
    .await;
    if let Err(e) = mdns.stop_browse(SERVICE_TYPE) {
        log(format!("link: could not stop mDNS browsing: {e}"));
    }
    result
}

async fn connect(addrs: &[SocketAddr]) -> Result<TcpStream, String> {
    let mut errors = Vec::new();
    for addr in addrs {
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(e)) => errors.push(format!("{addr}: {e}")),
            Err(_) => errors.push(format!("{addr}: timed out")),
        }
    }
    Err(format!(
        "could not connect to the tablet app ({})",
        errors.join(", ")
    ))
}

/// Reads one line into `line`. Returns false at end of stream; fails on lines over [`MAX_LINE`].
async fn read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    line: &mut String,
) -> std::io::Result<bool> {
    line.clear();
    let n = (&mut *reader)
        .take(MAX_LINE as u64 + 1)
        .read_line(line)
        .await?;
    if n > MAX_LINE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "message line too long",
        ));
    }
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_what_the_android_app_sends() {
        let hello = r#"{"type":"hello","width":3392,"height":2400,"rotation":90,"version":1,"app":"0.2.0"}"#;
        assert_eq!(
            serde_json::from_str::<TabletMessage>(hello).unwrap(),
            TabletMessage::Hello {
                version: 1,
                app: "0.2.0".into(),
                width: 3392,
                height: 2400,
                rotation: 90
            }
        );
        // Android's JSONObject writes whole doubles without a fraction.
        let edge = r#"{"type":"edge","edge":"top","state":"enter","along":1}"#;
        assert_eq!(
            serde_json::from_str::<TabletMessage>(edge).unwrap(),
            TabletMessage::Edge {
                edge: Side::Top,
                state: HoverState::Enter,
                along: 1.0
            }
        );
        let display = r#"{"type":"display","width":2400,"height":3392,"rotation":0}"#;
        assert!(matches!(
            serde_json::from_str::<TabletMessage>(display),
            Ok(TabletMessage::Display { .. })
        ));
    }

    #[test]
    fn rejects_unknown_or_malformed_messages() {
        for bad in [
            r#"{"type":"teleport"}"#,
            r#"{"type":"edge","edge":"middle","state":"enter","along":0.5}"#,
            r#"{"type":"edge","edge":"top","state":"enter"}"#,
            "not json",
        ] {
            assert!(serde_json::from_str::<TabletMessage>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn hello_matches_what_the_android_app_expects() {
        let hello = serde_json::to_string(&PcMessage::Hello {
            version: 1,
            edge: Side::Top,
        })
        .unwrap();
        assert_eq!(hello, r#"{"type":"hello","version":1,"edge":"top"}"#);
    }

    #[tokio::test]
    async fn reads_lines_until_end_of_stream() {
        let mut reader = &b"one\ntwo"[..];
        let mut line = String::new();
        assert!(read_line(&mut reader, &mut line).await.unwrap());
        assert_eq!(line, "one\n");
        assert!(read_line(&mut reader, &mut line).await.unwrap());
        assert_eq!(line, "two");
        assert!(!read_line(&mut reader, &mut line).await.unwrap());
    }

    #[tokio::test]
    async fn rejects_overlong_lines() {
        let long = format!("{}\n", "x".repeat(MAX_LINE + 10));
        let mut reader = long.as_bytes();
        let mut line = String::new();
        assert!(read_line(&mut reader, &mut line).await.is_err());
    }

    #[tokio::test]
    async fn accepts_a_line_of_exactly_the_limit() {
        let exact = format!("{}\n", "x".repeat(MAX_LINE - 1));
        let mut reader = exact.as_bytes();
        let mut line = String::new();
        assert!(read_line(&mut reader, &mut line).await.unwrap());
        assert_eq!(line.len(), MAX_LINE);
    }
}
