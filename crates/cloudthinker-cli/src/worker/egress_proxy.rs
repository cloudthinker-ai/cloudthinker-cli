use std::collections::BTreeSet;
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

const MAX_HEAD: usize = 16 * 1024;
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CONNECTIONS: usize = 512;
const RELOAD_INTERVAL: Duration = Duration::from_secs(1);

pub type ResolveFuture = Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send>>;
pub type Resolver = Arc<dyn Fn(String, u16) -> ResolveFuture + Send + Sync>;
pub type AddressPolicy = Arc<dyn Fn(IpAddr) -> bool + Send + Sync>;

pub fn system_resolver() -> Resolver {
    Arc::new(|host, port| {
        Box::pin(async move {
            Ok(tokio::net::lookup_host((host.as_str(), port))
                .await?
                .collect())
        })
    })
}

pub fn public_address_policy() -> AddressPolicy {
    Arc::new(is_public_ip)
}

pub struct ProxyConfig {
    pub allowlist_file: PathBuf,
    pub allowed_ports: BTreeSet<u16>,
    pub resolver: Resolver,
    pub address_allowed: AddressPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    Exact(String),
    Suffix(String),
}

impl Pattern {
    pub fn parse(line: &str) -> Option<Self> {
        let line = line.trim().to_ascii_lowercase();
        let line = line.strip_suffix('.').unwrap_or(&line);
        if let Some(rest) = line.strip_prefix("*.") {
            is_dns_name(rest).then(|| Pattern::Suffix(rest.to_string()))
        } else {
            is_dns_name(line).then(|| Pattern::Exact(line.to_string()))
        }
    }

    pub fn matches(&self, host: &str) -> bool {
        match self {
            Pattern::Exact(name) => host == name,
            Pattern::Suffix(base) => host
                .strip_suffix(base.as_str())
                .is_some_and(|prefix| prefix.len() > 1 && prefix.ends_with('.')),
        }
    }
}

pub fn parse_allowlist(text: &str) -> Vec<Pattern> {
    let mut warned = false;
    let mut patterns = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match Pattern::parse(line) {
            Some(pattern) => patterns.push(pattern),
            None if !warned => {
                warned = true;
                eprintln!("egress-proxy: skipping invalid allowlist entries");
            }
            None => {}
        }
    }
    patterns
}

pub fn allowlist_matches(patterns: &[Pattern], host: &str) -> bool {
    patterns.iter().any(|pattern| pattern.matches(host))
}

#[derive(Debug, PartialEq, Eq)]
pub enum HostError {
    IpLiteral,
    Invalid,
}

pub fn normalize_host(raw: &str) -> Result<String, HostError> {
    let mut host = raw.to_ascii_lowercase();
    if let Some(stripped) = host.strip_suffix('.') {
        host = stripped.to_string();
    }
    if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        host = inner.to_string();
    }
    if host.parse::<IpAddr>().is_ok() || looks_like_ipv4(&host) || host.contains(':') {
        return Err(HostError::IpLiteral);
    }
    if !is_dns_name(&host) {
        return Err(HostError::Invalid);
    }
    Ok(host)
}

fn looks_like_ipv4(host: &str) -> bool {
    host.rsplit('.')
        .find(|label| !label.is_empty())
        .is_some_and(|last| parse_inet_number(last).is_some())
}

fn parse_inet_number(part: &str) -> Option<u64> {
    if part.is_empty() {
        return None;
    }
    if let Some(hex) = part.strip_prefix("0x") {
        return if hex.is_empty() {
            Some(0)
        } else {
            u64::from_str_radix(hex, 16).ok()
        };
    }
    if part.len() > 1 && part.starts_with('0') {
        return u64::from_str_radix(&part[1..], 8).ok();
    }
    part.parse::<u64>().ok()
}

fn is_dns_name(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 || !host.contains('.') {
        return false;
    }
    host.split('.').all(|label| {
        (1..=63).contains(&label.len())
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    })
}

pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn in_v4(ip: Ipv4Addr, net: [u8; 4], prefix: u32) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    u32::from(ip) & mask == u32::from(Ipv4Addr::from(net)) & mask
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    const BLOCKED: [([u8; 4], u32); 14] = [
        ([0, 0, 0, 0], 8),
        ([10, 0, 0, 0], 8),
        ([100, 64, 0, 0], 10),
        ([127, 0, 0, 0], 8),
        ([169, 254, 0, 0], 16),
        ([172, 16, 0, 0], 12),
        ([192, 0, 0, 0], 24),
        ([192, 0, 2, 0], 24),
        ([192, 168, 0, 0], 16),
        ([198, 18, 0, 0], 15),
        ([198, 51, 100, 0], 24),
        ([203, 0, 113, 0], 24),
        ([224, 0, 0, 0], 4),
        ([240, 0, 0, 0], 4),
    ];
    !BLOCKED.iter().any(|(net, prefix)| in_v4(ip, *net, *prefix))
}

fn in_v6(ip: Ipv6Addr, net: Ipv6Addr, prefix: u32) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - prefix)
    };
    u128::from(ip) & mask == u128::from(net) & mask
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if in_v6(ip, Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0, 0), 96) {
        let [.., a, b, c, d] = ip.octets();
        return is_public_v4(Ipv4Addr::new(a, b, c, d));
    }
    let blocked: [(Ipv6Addr, u32); 9] = [
        (Ipv6Addr::UNSPECIFIED, 128),
        (Ipv6Addr::LOCALHOST, 128),
        (Ipv6Addr::new(0x64, 0xff9b, 0, 0, 0, 0, 0, 0), 96),
        (Ipv6Addr::new(0x100, 0, 0, 0, 0, 0, 0, 0), 64),
        (Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0), 32),
        (Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0), 7),
        (Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 10),
        (Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0, 0), 8),
        (Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0, 0), 96),
    ];
    !blocked.iter().any(|(net, prefix)| in_v6(ip, *net, *prefix))
}

struct Allowlist {
    path: PathBuf,
    state: Mutex<AllowlistState>,
}

struct AllowlistState {
    checked: Option<Instant>,
    stamp: Option<(SystemTime, u64)>,
    patterns: Arc<Vec<Pattern>>,
}

impl Allowlist {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            state: Mutex::new(AllowlistState {
                checked: None,
                stamp: None,
                patterns: Arc::new(Vec::new()),
            }),
        }
    }

    fn current(&self) -> Arc<Vec<Pattern>> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state
            .checked
            .is_some_and(|at| at.elapsed() < RELOAD_INTERVAL)
        {
            return state.patterns.clone();
        }
        state.checked = Some(Instant::now());
        let stamp = std::fs::metadata(&self.path)
            .ok()
            .and_then(|m| Some((m.modified().ok()?, m.len())));
        if stamp.is_none() {
            state.stamp = None;
            state.patterns = Arc::new(Vec::new());
        } else if stamp != state.stamp {
            state.patterns = Arc::new(match std::fs::read_to_string(&self.path) {
                Ok(text) => parse_allowlist(&text),
                Err(_) => Vec::new(),
            });
            state.stamp = stamp;
        }
        state.patterns.clone()
    }
}

struct Shared {
    config: ProxyConfig,
    allowlist: Allowlist,
}

pub async fn serve(listener: TcpListener, config: ProxyConfig) -> io::Result<()> {
    let shared = Arc::new(Shared {
        allowlist: Allowlist::new(config.allowlist_file.clone()),
        config,
    });
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let permit = permits
            .clone()
            .acquire_owned()
            .await
            .map_err(io::Error::other)?;
        let (stream, _) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(_) => continue,
        };
        let shared = shared.clone();
        tokio::spawn(async move {
            let _ = handle(stream, &shared).await;
            drop(permit);
        });
    }
}

pub async fn run(listen: SocketAddr, allowlist_file: PathBuf, ports: Vec<u16>) -> io::Result<()> {
    let listener = TcpListener::bind(listen).await?;
    let allowed_ports = if ports.is_empty() {
        BTreeSet::from([80, 443])
    } else {
        ports.into_iter().collect()
    };
    serve(
        listener,
        ProxyConfig {
            allowlist_file,
            allowed_ports,
            resolver: system_resolver(),
            address_allowed: public_address_policy(),
        },
    )
    .await
}

enum Deny {
    NotListed,
    Ip,
    Private,
    Unresolved,
    Port(u16),
}

impl Deny {
    fn reason(&self) -> &'static str {
        match self {
            Deny::NotListed => "not_listed",
            Deny::Ip => "ip_literal",
            Deny::Private => "private_address",
            Deny::Unresolved => "unresolved",
            Deny::Port(_) => "port",
        }
    }

    fn message(&self, host: &str) -> String {
        match self {
            Deny::NotListed => format!(
                "Blocked by the Worker egress policy: {host} is not on the allowlist. \
                 Ask a workspace admin to add it."
            ),
            Deny::Ip => {
                "Blocked by the Worker egress policy: connect by host name, not by IP address."
                    .into()
            }
            Deny::Private => format!(
                "Blocked by the Worker egress policy: {host} resolves to a private or metadata address."
            ),
            Deny::Unresolved => {
                format!("Blocked by the Worker egress policy: {host} did not resolve.")
            }
            Deny::Port(port) => format!(
                "Blocked by the Worker egress policy: port {port} is not allowed. Use HTTPS on port 443."
            ),
        }
    }
}

fn log_decision(host: &str, port: u16, decision: &str, reason: &str) {
    let line = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "event": "egress",
        "host": host,
        "port": port,
        "decision": decision,
        "reason": reason,
    });
    eprintln!("{line}");
}

async fn respond<W: AsyncWrite + Unpin>(
    stream: &mut W,
    status: &str,
    body: &str,
) -> io::Result<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}\n",
        body.len() + 1
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

struct Head {
    method: String,
    target: String,
    headers: Vec<(String, Vec<u8>)>,
    rest: Vec<u8>,
}

async fn read_head<R: AsyncRead + Unpin>(stream: &mut R) -> io::Result<Option<Head>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    loop {
        let mut headers = [httparse::EMPTY_HEADER; 100];
        let mut request = httparse::Request::new(&mut headers);
        match request.parse(&buf) {
            Ok(httparse::Status::Complete(len)) => {
                return Ok(Some(Head {
                    method: request.method.unwrap_or_default().to_string(),
                    target: request.path.unwrap_or_default().to_string(),
                    headers: request
                        .headers
                        .iter()
                        .map(|h| (h.name.to_string(), h.value.to_vec()))
                        .collect(),
                    rest: buf[len..].to_vec(),
                }));
            }
            Ok(httparse::Status::Partial) => {}
            Err(_) => return Ok(None),
        }
        if buf.len() >= MAX_HEAD {
            return Ok(None);
        }
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > MAX_HEAD {
            buf.truncate(MAX_HEAD);
        }
    }
}

fn split_authority(authority: &str, default_port: Option<u16>) -> Option<(String, u16)> {
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    if let Some(rest) = authority.strip_prefix('[') {
        let (inner, after) = rest.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(port) => port.parse().ok()?,
            None if after.is_empty() => default_port?,
            None => return None,
        };
        return Some((format!("[{inner}]"), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Some((host.to_string(), port.parse().ok()?)),
        None => Some((authority.to_string(), default_port?)),
    }
}

async fn handle(mut client: TcpStream, shared: &Shared) -> io::Result<()> {
    let head = match tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut client)).await {
        Ok(Ok(Some(head))) => head,
        Ok(Ok(None)) => return respond(&mut client, "400 Bad Request", "Bad request.").await,
        _ => return Ok(()),
    };
    let connect = head.method.eq_ignore_ascii_case("CONNECT");
    let parsed = if connect {
        split_authority(&head.target, None).map(|(h, p)| (h, p, String::new()))
    } else {
        head.target.strip_prefix("http://").and_then(|rest| {
            let split = rest.find(['/', '?']).unwrap_or(rest.len());
            let (authority, path) = rest.split_at(split);
            let path = if path.is_empty() {
                "/".to_string()
            } else if path.starts_with('?') {
                format!("/{path}")
            } else {
                path.to_string()
            };
            split_authority(authority, Some(80)).map(|(h, p)| (h, p, path))
        })
    };
    let Some((raw_host, port, path)) = parsed else {
        return respond(
            &mut client,
            "400 Bad Request",
            "Bad request: use CONNECT or an absolute http:// URL.",
        )
        .await;
    };
    let addrs = match check(shared, &raw_host, port).await {
        Ok((host, addrs)) => {
            log_decision(&host, port, "allow", "allowlisted");
            addrs
        }
        Err((host, deny)) => {
            log_decision(&host, port, "deny", deny.reason());
            let message = deny.message(&host);
            let status = format!("403 {}", message.replace(char::is_control, " "));
            return respond(&mut client, &status, &message).await;
        }
    };
    let Some(mut upstream) = connect_any(&addrs).await else {
        return respond(
            &mut client,
            "502 Bad Gateway",
            "Upstream connection failed.",
        )
        .await;
    };
    if connect {
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        if !head.rest.is_empty() {
            upstream.write_all(&head.rest).await?;
        }
    } else {
        upstream.write_all(&rewrite_request(&head, &path)).await?;
        upstream.write_all(&head.rest).await?;
    }
    tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}

fn rewrite_request(head: &Head, path: &str) -> Vec<u8> {
    let named: Vec<String> = head
        .headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("connection"))
        .flat_map(|(_, value)| {
            String::from_utf8_lossy(value)
                .split(',')
                .map(|v| v.trim().to_ascii_lowercase())
                .collect::<Vec<_>>()
        })
        .collect();
    let mut out = format!("{} {} HTTP/1.1\r\n", head.method, path).into_bytes();
    for (name, value) in &head.headers {
        let lower = name.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "connection" | "keep-alive" | "te" | "trailer" | "upgrade"
        ) || lower.starts_with("proxy-")
            || named.contains(&lower)
        {
            continue;
        }
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"Connection: close\r\n\r\n");
    out
}

async fn check(
    shared: &Shared,
    raw_host: &str,
    port: u16,
) -> Result<(String, Vec<SocketAddr>), (String, Deny)> {
    let host = match normalize_host(raw_host) {
        Ok(host) => host,
        Err(HostError::IpLiteral) => return Err((raw_host.to_ascii_lowercase(), Deny::Ip)),
        Err(HostError::Invalid) => return Err((raw_host.to_ascii_lowercase(), Deny::NotListed)),
    };
    if !shared.config.allowed_ports.contains(&port) {
        return Err((host, Deny::Port(port)));
    }
    if !allowlist_matches(&shared.allowlist.current(), &host) {
        return Err((host, Deny::NotListed));
    }
    let addrs = match (shared.config.resolver)(host.clone(), port).await {
        Ok(addrs) if !addrs.is_empty() => addrs,
        _ => return Err((host, Deny::Unresolved)),
    };
    if addrs
        .iter()
        .any(|addr| !(shared.config.address_allowed)(addr.ip()))
    {
        return Err((host, Deny::Private));
    }
    Ok((host, addrs))
}

async fn connect_any(addrs: &[SocketAddr]) -> Option<TcpStream> {
    for addr in addrs {
        if let Ok(Ok(stream)) =
            tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await
        {
            return Some(stream);
        }
    }
    None
}

#[cfg(test)]
#[path = "egress_proxy_tests.rs"]
mod tests;
