use std::fmt;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_PAYLOAD: usize = 16 * 1024 * 1024;
pub const OUTPUT_CHUNK: usize = 64 * 1024;
pub const MAX_UPLOAD: usize = MAX_PAYLOAD - 4096;

const HELLO: u8 = 1;
const WELCOME: u8 = 2;
const INPUT: u8 = 3;
const RESIZE: u8 = 4;
const UPLOAD: u8 = 5;
const OUTPUT: u8 = 6;
const PING: u8 = 7;
const NOTICE: u8 = 8;
const EXIT: u8 = 9;
const REPLACED: u8 = 10;
const QUERY: u8 = 11;
const CATALOG: u8 = 12;
const COMPLETE: u8 = 13;
const COMPLETIONS: u8 = 14;
const DETACH: u8 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub version: u16,
    pub size: Size,
    pub name: String,
    pub cwd: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub resume: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachState {
    Started,
    Attached,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Welcome {
    pub version: u16,
    pub cli_version: String,
    pub state: AttachState,
    pub started_at: u64,
    pub cwd: String,
    pub session: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub probe: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningAgent {
    pub name: String,
    pub cwd: String,
    pub started_at: u64,
    pub title: String,
    pub origin: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentFolder {
    pub cwd: String,
    pub title: String,
    pub modified: u64,
    pub session: String,
    pub origin: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    pub home: String,
    pub probe: Option<String>,
    pub running: Vec<RunningAgent>,
    pub recent: Vec<RecentFolder>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadKind {
    Image,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upload {
    pub kind: UploadKind,
    pub name: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Hello(Hello),
    Welcome(Welcome),
    Input(Vec<u8>),
    Resize(Size),
    Upload(Upload),
    Output(Vec<u8>),
    Ping,
    Notice(String),
    Exit(i32),
    Replaced,
    Query(Query),
    Catalog(Catalog),
    Complete(String),
    Completions(Vec<String>),
    Detach,
}

#[derive(Debug)]
pub enum WireError {
    Io(std::io::Error),
    TooLarge(usize),
    UnknownKind(u8),
    Malformed(&'static str),
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "connection error: {error}"),
            Self::TooLarge(len) => write!(f, "frame of {len} bytes is over the 16 MiB limit"),
            Self::UnknownKind(kind) => write!(
                f,
                "unknown frame kind {kind}; update cloudthinker on both machines"
            ),
            Self::Malformed(what) => write!(f, "malformed {what} frame"),
        }
    }
}

impl From<std::io::Error> for WireError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::new();
        let kind = match self {
            Self::Hello(hello) => {
                put_u16(&mut body, hello.version);
                put_size(&mut body, hello.size);
                put_str(&mut body, &hello.name);
                match &hello.cwd {
                    Some(cwd) => {
                        body.push(1);
                        put_str(&mut body, cwd);
                    }
                    None => body.push(0),
                }
                put_len(&mut body, hello.args.len());
                for arg in &hello.args {
                    put_str(&mut body, arg);
                }
                put_len(&mut body, hello.env.len());
                for (key, value) in &hello.env {
                    put_str(&mut body, key);
                    put_str(&mut body, value);
                }
                match hello.resume {
                    Some(session) => {
                        body.push(1);
                        put_u64(&mut body, session);
                    }
                    None => body.push(0),
                }
                HELLO
            }
            Self::Welcome(welcome) => {
                put_u16(&mut body, welcome.version);
                put_str(&mut body, &welcome.cli_version);
                body.push(match welcome.state {
                    AttachState::Started => 0,
                    AttachState::Attached => 1,
                });
                put_u64(&mut body, welcome.started_at);
                put_str(&mut body, &welcome.cwd);
                put_u64(&mut body, welcome.session);
                WELCOME
            }
            Self::Input(bytes) => {
                body.extend_from_slice(bytes);
                INPUT
            }
            Self::Resize(size) => {
                put_size(&mut body, *size);
                RESIZE
            }
            Self::Upload(upload) => {
                body.push(match upload.kind {
                    UploadKind::Image => 0,
                    UploadKind::File => 1,
                });
                put_str(&mut body, &upload.name);
                body.extend_from_slice(&upload.bytes);
                UPLOAD
            }
            Self::Output(bytes) => {
                body.extend_from_slice(bytes);
                OUTPUT
            }
            Self::Ping => PING,
            Self::Notice(text) => {
                body.extend_from_slice(text.as_bytes());
                NOTICE
            }
            Self::Exit(code) => {
                body.extend_from_slice(&code.to_be_bytes());
                EXIT
            }
            Self::Replaced => REPLACED,
            Self::Query(query) => {
                put_opt(&mut body, query.probe.as_deref());
                QUERY
            }
            Self::Catalog(catalog) => {
                put_str(&mut body, &catalog.home);
                put_opt(&mut body, catalog.probe.as_deref());
                put_len(&mut body, catalog.running.len());
                for agent in &catalog.running {
                    put_str(&mut body, &agent.name);
                    put_str(&mut body, &agent.cwd);
                    put_u64(&mut body, agent.started_at);
                    put_str(&mut body, &agent.title);
                    put_opt(&mut body, agent.origin.as_deref());
                }
                put_len(&mut body, catalog.recent.len());
                for folder in &catalog.recent {
                    put_str(&mut body, &folder.cwd);
                    put_str(&mut body, &folder.title);
                    put_u64(&mut body, folder.modified);
                    put_str(&mut body, &folder.session);
                    put_opt(&mut body, folder.origin.as_deref());
                }
                CATALOG
            }
            Self::Complete(prefix) => {
                put_str(&mut body, prefix);
                COMPLETE
            }
            Self::Completions(entries) => {
                put_len(&mut body, entries.len());
                for entry in entries {
                    put_str(&mut body, entry);
                }
                COMPLETIONS
            }
            Self::Detach => DETACH,
        };
        let mut frame = Vec::with_capacity(body.len() + 5);
        frame.push(kind);
        put_len(&mut frame, body.len());
        frame.extend_from_slice(&body);
        frame
    }

    pub fn decode(kind: u8, payload: &[u8]) -> Result<Self, WireError> {
        let mut cursor = Cursor {
            bytes: payload,
            at: 0,
        };
        let frame = match kind {
            HELLO => {
                let version = cursor.u16("hello")?;
                let size = cursor.size("hello")?;
                let name = cursor.string("hello")?;
                let cwd = match cursor.u8("hello")? {
                    0 => None,
                    _ => Some(cursor.string("hello")?),
                };
                let mut args = Vec::new();
                for _ in 0..cursor.u32("hello")? {
                    args.push(cursor.string("hello")?);
                }
                let mut env = Vec::new();
                for _ in 0..cursor.u32("hello")? {
                    env.push((cursor.string("hello")?, cursor.string("hello")?));
                }
                let resume = match cursor.u8("hello")? {
                    0 => None,
                    _ => Some(cursor.u64("hello")?),
                };
                Self::Hello(Hello {
                    version,
                    size,
                    name,
                    cwd,
                    args,
                    env,
                    resume,
                })
            }
            WELCOME => Self::Welcome(Welcome {
                version: cursor.u16("welcome")?,
                cli_version: cursor.string("welcome")?,
                state: match cursor.u8("welcome")? {
                    0 => AttachState::Started,
                    _ => AttachState::Attached,
                },
                started_at: cursor.u64("welcome")?,
                cwd: cursor.string("welcome")?,
                session: cursor.u64("welcome")?,
            }),
            INPUT => Self::Input(payload.to_vec()),
            RESIZE => Self::Resize(cursor.size("resize")?),
            UPLOAD => {
                let kind = match cursor.u8("upload")? {
                    0 => UploadKind::Image,
                    _ => UploadKind::File,
                };
                let name = cursor.string("upload")?;
                Self::Upload(Upload {
                    kind,
                    name,
                    bytes: cursor.rest().to_vec(),
                })
            }
            OUTPUT => Self::Output(payload.to_vec()),
            PING => Self::Ping,
            NOTICE => Self::Notice(String::from_utf8_lossy(payload).into_owned()),
            EXIT => {
                let bytes: [u8; 4] = cursor
                    .take(4, "exit")?
                    .try_into()
                    .map_err(|_| WireError::Malformed("exit"))?;
                Self::Exit(i32::from_be_bytes(bytes))
            }
            REPLACED => Self::Replaced,
            QUERY => Self::Query(Query {
                probe: cursor.opt("query")?,
            }),
            CATALOG => {
                let home = cursor.string("catalog")?;
                let probe = cursor.opt("catalog")?;
                let mut running = Vec::new();
                for _ in 0..cursor.u32("catalog")? {
                    running.push(RunningAgent {
                        name: cursor.string("catalog")?,
                        cwd: cursor.string("catalog")?,
                        started_at: cursor.u64("catalog")?,
                        title: cursor.string("catalog")?,
                        origin: cursor.opt("catalog")?,
                    });
                }
                let mut recent = Vec::new();
                for _ in 0..cursor.u32("catalog")? {
                    recent.push(RecentFolder {
                        cwd: cursor.string("catalog")?,
                        title: cursor.string("catalog")?,
                        modified: cursor.u64("catalog")?,
                        session: cursor.string("catalog")?,
                        origin: cursor.opt("catalog")?,
                    });
                }
                Self::Catalog(Catalog {
                    home,
                    probe,
                    running,
                    recent,
                })
            }
            COMPLETE => Self::Complete(cursor.string("complete")?),
            COMPLETIONS => {
                let mut entries = Vec::new();
                for _ in 0..cursor.u32("completions")? {
                    entries.push(cursor.string("completions")?);
                }
                Self::Completions(entries)
            }
            DETACH => Self::Detach,
            other => return Err(WireError::UnknownKind(other)),
        };
        Ok(frame)
    }
}
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Option<Frame>, WireError> {
    let mut header = [0u8; 5];
    let mut filled = 0;
    while filled < header.len() {
        let read = reader.read(&mut header[filled..]).await?;
        if read == 0 {
            return if filled == 0 {
                Ok(None)
            } else {
                Err(WireError::Malformed("truncated header"))
            };
        }
        filled += read;
    }
    let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if len > MAX_PAYLOAD {
        return Err(WireError::TooLarge(len));
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await?;
    Frame::decode(header[0], &payload).map(Some)
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &Frame,
) -> Result<(), WireError> {
    let bytes = frame.encode();
    if bytes.len() - 5 > MAX_PAYLOAD {
        return Err(WireError::TooLarge(bytes.len() - 5));
    }
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}
pub fn output_frames(bytes: &[u8]) -> impl Iterator<Item = Frame> + '_ {
    bytes
        .chunks(OUTPUT_CHUNK)
        .map(|chunk| Frame::Output(chunk.to_vec()))
}

fn put_u16(body: &mut Vec<u8>, value: u16) {
    body.extend_from_slice(&value.to_be_bytes());
}

fn put_len(body: &mut Vec<u8>, len: usize) {
    body.extend_from_slice(&u32::try_from(len).unwrap_or(u32::MAX).to_be_bytes());
}

fn put_u64(body: &mut Vec<u8>, value: u64) {
    body.extend_from_slice(&value.to_be_bytes());
}

fn put_opt(body: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            body.push(1);
            put_str(body, value);
        }
        None => body.push(0),
    }
}

fn put_size(body: &mut Vec<u8>, size: Size) {
    put_u16(body, size.rows);
    put_u16(body, size.cols);
}

fn put_str(body: &mut Vec<u8>, value: &str) {
    put_len(body, value.len());
    body.extend_from_slice(value.as_bytes());
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, len: usize, what: &'static str) -> Result<&'a [u8], WireError> {
        let end = self.at.checked_add(len).ok_or(WireError::Malformed(what))?;
        let slice = self
            .bytes
            .get(self.at..end)
            .ok_or(WireError::Malformed(what))?;
        self.at = end;
        Ok(slice)
    }

    fn u8(&mut self, what: &'static str) -> Result<u8, WireError> {
        Ok(self.take(1, what)?[0])
    }

    fn u16(&mut self, what: &'static str) -> Result<u16, WireError> {
        let bytes = self.take(2, what)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self, what: &'static str) -> Result<u32, WireError> {
        let bytes = self.take(4, what)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self, what: &'static str) -> Result<u64, WireError> {
        let bytes: [u8; 8] = self
            .take(8, what)?
            .try_into()
            .map_err(|_| WireError::Malformed(what))?;
        Ok(u64::from_be_bytes(bytes))
    }

    fn opt(&mut self, what: &'static str) -> Result<Option<String>, WireError> {
        match self.u8(what)? {
            0 => Ok(None),
            _ => self.string(what).map(Some),
        }
    }

    fn size(&mut self, what: &'static str) -> Result<Size, WireError> {
        Ok(Size {
            rows: self.u16(what)?,
            cols: self.u16(what)?,
        })
    }

    fn string(&mut self, what: &'static str) -> Result<String, WireError> {
        let len = self.u32(what)? as usize;
        let bytes = self.take(len, what)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| WireError::Malformed(what))
    }

    fn rest(&mut self) -> &'a [u8] {
        let rest = self.bytes.get(self.at..).unwrap_or_default();
        self.at = self.bytes.len();
        rest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn every_frame_round_trips_through_a_stream() {
        let frames = vec![
            Frame::Hello(Hello {
                version: PROTOCOL_VERSION,
                size: Size {
                    rows: 40,
                    cols: 140,
                },
                name: "default".into(),
                cwd: Some("/srv/app".into()),
                args: vec!["-c".into(), "two words".into()],
                env: vec![("TERM".into(), "xterm-256color".into())],
                resume: Some(42),
            }),
            Frame::Welcome(Welcome {
                version: 1,
                cli_version: "0.8.3".into(),
                state: AttachState::Attached,
                started_at: 1_760_000_000,
                cwd: "~/app".into(),
                session: 42,
            }),
            Frame::Input(b"\x1b[118;5u".to_vec()),
            Frame::Resize(Size {
                rows: 30,
                cols: 100,
            }),
            Frame::Upload(Upload {
                kind: UploadKind::Image,
                name: "clipboard.png".into(),
                bytes: vec![0, 1, 2],
            }),
            Frame::Output(b"\x1b]52;c;aGk=\x07".to_vec()),
            Frame::Ping,
            Frame::Notice("Attached".into()),
            Frame::Exit(-1),
            Frame::Replaced,
            Frame::Query(Query {
                probe: Some("~/app".into()),
            }),
            Frame::Query(Query { probe: None }),
            Frame::Catalog(Catalog {
                home: "/home/me".into(),
                probe: Some("/home/me/app".into()),
                running: vec![RunningAgent {
                    name: "app-1a2b3c".into(),
                    cwd: "/home/me/app".into(),
                    started_at: 1_760_000_000,
                    title: "fix login bug".into(),
                    origin: Some("gitlab.com/team/app".into()),
                }],
                recent: vec![RecentFolder {
                    cwd: "/srv/infra".into(),
                    title: String::new(),
                    modified: 1_759_000_000,
                    session: "/home/me/.cloudthinker/agent/sessions/--srv-infra--/a.jsonl".into(),
                    origin: None,
                }],
            }),
            Frame::Complete("~/ap".into()),
            Frame::Completions(vec!["~/app/".into(), "~/apps/".into()]),
            Frame::Detach,
        ];
        let mut stream = Vec::new();
        for frame in &frames {
            write_frame(&mut stream, frame).await.unwrap();
        }
        let mut reader = stream.as_slice();
        for frame in &frames {
            assert_eq!(read_frame(&mut reader).await.unwrap().as_ref(), Some(frame));
        }
        assert!(read_frame(&mut reader).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_frame_over_the_cap_is_refused_before_allocation() {
        let mut bytes = vec![OUTPUT];
        bytes.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(matches!(
            read_frame(&mut bytes.as_slice()).await,
            Err(WireError::TooLarge(_))
        ));
    }

    #[test]
    fn output_is_split_into_bounded_frames() {
        let bytes = vec![b'x'; OUTPUT_CHUNK * 2 + 1];
        let sizes: Vec<usize> = output_frames(&bytes)
            .map(|frame| match frame {
                Frame::Output(chunk) => chunk.len(),
                _ => 0,
            })
            .collect();
        assert_eq!(sizes, [OUTPUT_CHUNK, OUTPUT_CHUNK, 1]);
    }

    #[test]
    fn truncated_payloads_are_malformed_not_panics() {
        assert!(matches!(
            Frame::decode(HELLO, &[0, 1]),
            Err(WireError::Malformed(_))
        ));
        assert!(matches!(
            Frame::decode(EXIT, &[0]),
            Err(WireError::Malformed(_))
        ));
        assert!(matches!(
            Frame::decode(99, &[]),
            Err(WireError::UnknownKind(99))
        ));
    }
}
