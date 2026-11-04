//! `tubarr --healthcheck`: asks the running server's `/healthz` on loopback and exits 0 or 1.
//!
//! The container image has no curl, so Docker's HEALTHCHECK runs the binary itself. A plain
//! HTTP/1.0 request over a TCP socket is all it takes, without building an HTTP client.

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

/// Why the probe failed.
#[derive(Debug, thiserror::Error)]
pub enum HealthcheckError {
    #[error("cannot reach {addr}: {source}")]
    Connect {
        addr: SocketAddr,
        source: std::io::Error,
    },
    #[error("request to {addr} failed: {source}")]
    Io {
        addr: SocketAddr,
        source: std::io::Error,
    },
    #[error("no answer from {addr} within {timeout:?}")]
    Timeout { addr: SocketAddr, timeout: Duration },
    #[error("{addr}/healthz answered {status:?}")]
    Status { addr: SocketAddr, status: String },
}

/// The address to probe for a server bound to `listen`: a wildcard bind is reached on loopback.
pub fn probe_addr(listen: SocketAddr) -> SocketAddr {
    let ip = match listen.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    SocketAddr::new(ip, listen.port())
}

/// GETs `/healthz` on `addr` and succeeds only on a 200 within `timeout`.
pub async fn probe(addr: SocketAddr, timeout: Duration) -> Result<(), HealthcheckError> {
    tokio::time::timeout(timeout, request(addr))
        .await
        .map_err(|_| HealthcheckError::Timeout { addr, timeout })?
}

async fn request(addr: SocketAddr) -> Result<(), HealthcheckError> {
    let mut stream = TcpStream::connect(addr)
        .await
        .map_err(|source| HealthcheckError::Connect { addr, source })?;
    let io = |source| HealthcheckError::Io { addr, source };
    stream
        .write_all(b"GET /healthz HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .map_err(io)?;
    // The status line is all that matters, and it comes first.
    let mut head = [0u8; 64];
    let mut len = 0;
    while len < head.len() {
        let n = stream.read(&mut head[len..]).await.map_err(io)?;
        if n == 0 {
            break;
        }
        len += n;
        if head[..len].contains(&b'\n') {
            break;
        }
    }
    let line = String::from_utf8_lossy(&head[..len]);
    let status = line.lines().next().unwrap_or_default().trim().to_owned();
    let mut parts = status.split_whitespace();
    let is_http = parts.next().is_some_and(|v| v.starts_with("HTTP/"));
    if is_http && parts.next() == Some("200") {
        Ok(())
    } else {
        Err(HealthcheckError::Status { addr, status })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    async fn serve_once(reply: &'static [u8]) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 256];
            let _ = socket.read(&mut buf).await.unwrap();
            socket.write_all(reply).await.unwrap();
        });
        addr
    }

    #[test]
    fn wildcard_binds_are_probed_on_loopback() {
        assert_eq!(
            probe_addr("0.0.0.0:8480".parse().unwrap()),
            "127.0.0.1:8480".parse().unwrap()
        );
        assert_eq!(
            probe_addr("[::]:8480".parse().unwrap()),
            "[::1]:8480".parse().unwrap()
        );
        assert_eq!(
            probe_addr("10.1.2.3:9000".parse().unwrap()),
            "10.1.2.3:9000".parse().unwrap()
        );
    }

    #[tokio::test]
    async fn ok_on_200() {
        let addr = serve_once(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok").await;
        probe(addr, Duration::from_secs(5)).await.unwrap();
    }

    #[tokio::test]
    async fn fails_on_other_status() {
        let addr = serve_once(b"HTTP/1.1 503 Service Unavailable\r\n\r\n").await;
        let err = probe(addr, Duration::from_secs(5)).await.unwrap_err();
        assert!(matches!(err, HealthcheckError::Status { .. }), "{err}");
    }

    #[tokio::test]
    async fn fails_when_nothing_listens() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let err = probe(addr, Duration::from_secs(5)).await.unwrap_err();
        assert!(matches!(err, HealthcheckError::Connect { .. }), "{err}");
    }

    #[tokio::test]
    async fn an_axum_server_passes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().route("/healthz", axum::routing::get(|| async { "ok" }));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        probe(addr, Duration::from_secs(5)).await.unwrap();
    }
}
