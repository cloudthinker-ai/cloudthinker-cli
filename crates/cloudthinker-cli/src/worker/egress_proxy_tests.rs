use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn normalizes_host_names() {
    assert_eq!(normalize_host("Example.COM."), Ok("example.com".into()));
    assert_eq!(
        normalize_host("a_b.example.com"),
        Ok("a_b.example.com".into())
    );
    assert_eq!(normalize_host("localhost"), Err(HostError::Invalid));
    assert_eq!(normalize_host("bad host.com"), Err(HostError::Invalid));
    assert_eq!(
        normalize_host(&format!("{}.com", "a".repeat(64))),
        Err(HostError::Invalid)
    );
}

#[test]
fn rejects_ip_literals_in_every_form() {
    for raw in [
        "127.0.0.1",
        "[::1]",
        "::1",
        "2130706433",
        "0x7f.1",
        "127.1",
        "0177.0.0.1",
        "0x7f000001",
        "10.0.0.1.",
    ] {
        assert_eq!(normalize_host(raw), Err(HostError::IpLiteral), "{raw}");
    }
}

#[test]
fn matches_allowlist_patterns() {
    let patterns = parse_allowlist("# comment\n\nexample.com\n*.github.com\nnot valid\n");
    assert_eq!(patterns.len(), 2);
    assert!(allowlist_matches(&patterns, "example.com"));
    assert!(!allowlist_matches(&patterns, "www.example.com"));
    assert!(allowlist_matches(&patterns, "api.github.com"));
    assert!(allowlist_matches(&patterns, "a.b.c.github.com"));
    assert!(!allowlist_matches(&patterns, "github.com"));
    assert!(!allowlist_matches(&patterns, "evilgithub.com"));
    assert!(!allowlist_matches(&[], "example.com"));
}

#[test]
fn classifies_non_public_addresses() {
    for ip in [
        "0.1.2.3",
        "10.1.1.1",
        "100.64.0.1",
        "100.127.255.255",
        "127.0.0.1",
        "169.254.169.254",
        "172.16.0.1",
        "172.31.255.255",
        "192.0.0.8",
        "192.0.2.1",
        "192.168.1.1",
        "198.18.0.1",
        "198.19.255.255",
        "198.51.100.1",
        "203.0.113.1",
        "224.0.0.1",
        "240.0.0.1",
        "255.255.255.255",
        "::",
        "::1",
        "::ffff:127.0.0.1",
        "::ffff:169.254.169.254",
        "64:ff9b::a00:1",
        "100::1",
        "2001:db8::1",
        "fc00::1",
        "fd00:ec2::254",
        "fe80::1",
        "ff02::1",
    ] {
        assert!(!is_public_ip(ip.parse().unwrap()), "{ip}");
    }
    for ip in [
        "93.184.216.34",
        "8.8.8.8",
        "172.32.0.1",
        "::ffff:8.8.8.8",
        "2606:4700::1111",
    ] {
        assert!(is_public_ip(ip.parse().unwrap()), "{ip}");
    }
}

async fn send(proxy: SocketAddr, request: &[u8]) -> TcpStream {
    let mut stream = TcpStream::connect(proxy).await.unwrap();
    stream.write_all(request).await.unwrap();
    stream
}

async fn read_all(mut stream: TcpStream) -> String {
    let mut out = Vec::new();
    stream.read_to_end(&mut out).await.unwrap();
    String::from_utf8_lossy(&out).into_owned()
}

#[tokio::test]
async fn proxies_allowed_hosts_and_blocks_others() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = upstream.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).await.unwrap();
                let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                if text.starts_with("GET ") {
                    let body = text.lines().next().unwrap_or_default().to_string();
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    stream.write_all(reply.as_bytes()).await.unwrap();
                } else {
                    stream.write_all(&buf[..n]).await.unwrap();
                }
            });
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let allowlist = dir.path().join("allowlist");
    std::fs::write(&allowlist, "allowed.test\n").unwrap();
    let resolver: Resolver =
        Arc::new(move |_host, _port| Box::pin(async move { Ok(vec![upstream_addr]) }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = listener.local_addr().unwrap();
    let port = upstream_addr.port();
    tokio::spawn(serve(
        listener,
        ProxyConfig {
            allowlist_file: allowlist.clone(),
            allowed_ports: BTreeSet::from([port]),
            resolver,
            address_allowed: Arc::new(|ip: IpAddr| ip.is_loopback()),
        },
    ));

    let mut tunnel = send(
        proxy,
        format!("CONNECT allowed.test:{port} HTTP/1.1\r\n\r\n").as_bytes(),
    )
    .await;
    let mut buf = [0u8; 256];
    let n = tunnel.read(&mut buf).await.unwrap();
    assert!(String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 200"));
    tunnel.write_all(b"ping").await.unwrap();
    let n = tunnel.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"ping");

    let denied = read_all(
        send(
            proxy,
            format!("CONNECT denied.test:{port} HTTP/1.1\r\n\r\n").as_bytes(),
        )
        .await,
    )
    .await;
    assert!(denied.starts_with("HTTP/1.1 403 Blocked by the Worker egress policy: "));
    assert!(denied.contains(
        "Blocked by the Worker egress policy: denied.test is not on the allowlist. Ask a workspace admin to add it."
    ));

    let request = format!(
        "GET http://allowed.test:{port}/path?q=1 HTTP/1.1\r\n\
         Host: allowed.test\r\nProxy-Authorization: x\r\n\r\n"
    );
    let forwarded = read_all(send(proxy, request.as_bytes()).await).await;
    assert!(forwarded.starts_with("HTTP/1.1 200 OK"));
    assert!(forwarded.ends_with("GET /path?q=1 HTTP/1.1"));

    tokio::time::sleep(Duration::from_millis(1100)).await;
    std::fs::write(&allowlist, "other.test\n").unwrap();
    let revoked = read_all(
        send(
            proxy,
            format!("CONNECT allowed.test:{port} HTTP/1.1\r\n\r\n").as_bytes(),
        )
        .await,
    )
    .await;
    assert!(revoked.starts_with("HTTP/1.1 403 Blocked by the Worker egress policy: "));
}

#[tokio::test]
async fn a_host_that_does_not_resolve_is_blocked_as_unresolved() {
    let dir = tempfile::tempdir().unwrap();
    let allowlist = dir.path().join("allowlist");
    std::fs::write(&allowlist, "missing.test\n").unwrap();
    let resolver: Resolver = Arc::new(|_host, _port| {
        Box::pin(async { Err(io::Error::new(io::ErrorKind::NotFound, "no such host")) })
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = listener.local_addr().unwrap();
    tokio::spawn(serve(
        listener,
        ProxyConfig {
            allowlist_file: allowlist,
            allowed_ports: BTreeSet::from([443]),
            resolver,
            address_allowed: public_address_policy(),
        },
    ));

    let reply = read_all(send(proxy, b"CONNECT missing.test:443 HTTP/1.1\r\n\r\n").await).await;

    assert!(reply.starts_with("HTTP/1.1 403 Blocked by the Worker egress policy: "));
    assert!(reply.contains("Blocked by the Worker egress policy: missing.test did not resolve."));
    assert!(!reply.contains("private"));
}
