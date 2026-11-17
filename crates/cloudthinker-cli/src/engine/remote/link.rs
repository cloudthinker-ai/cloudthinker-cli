use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::wire::{AttachState, Size, WireError};

pub const LINK_VERSION: u16 = 1;
const MAX_LINK_PAYLOAD: usize = 1024 * 1024;

const OPEN: u8 = 1;
const INPUT: u8 = 2;
const RESIZE: u8 = 3;
const READY: u8 = 11;
const OUTPUT: u8 = 12;
const EXIT: u8 = 13;
const REPLACED: u8 = 14;
const DETACH: u8 = 15;
const FAILED: u8 = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Start {
    pub cwd: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Open {
    pub version: u16,
    pub size: Size,
    pub resume: Option<u64>,
    pub start: Option<Start>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ready {
    pub state: AttachState,
    pub pid: Option<u32>,
    pub started_at: u64,
    pub cwd: String,
    pub session: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToHost {
    Open(Open),
    Input(Vec<u8>),
    Resize(Size),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromHost {
    Ready(Ready),
    Output(Vec<u8>),
    Exit(i32),
    Replaced,
    Detach,
    Failed(String),
}

impl ToHost {
    fn encode(&self) -> (u8, Vec<u8>) {
        let mut body = Vec::new();
        let kind = match self {
            Self::Open(open) => {
                put_u16(&mut body, open.version);
                put_size(&mut body, open.size);
                put_u64(&mut body, open.resume.unwrap_or_default());
                body.push(u8::from(open.resume.is_some()));
                match &open.start {
                    Some(start) => {
                        body.push(1);
                        put_str(&mut body, &start.cwd);
                        put_u32(&mut body, start.args.len());
                        for arg in &start.args {
                            put_str(&mut body, arg);
                        }
                        put_u32(&mut body, start.env.len());
                        for (key, value) in &start.env {
                            put_str(&mut body, key);
                            put_str(&mut body, value);
                        }
                    }
                    None => body.push(0),
                }
                OPEN
            }
            Self::Input(bytes) => {
                body.extend_from_slice(bytes);
                INPUT
            }
            Self::Resize(size) => {
                put_size(&mut body, *size);
                RESIZE
            }
        };
        (kind, body)
    }

    fn decode(kind: u8, body: &[u8]) -> Result<Self, WireError> {
        let mut cursor = Cursor { body, at: 0 };
        let frame = match kind {
            OPEN => {
                let version = cursor.u16()?;
                let size = cursor.size()?;
                let resume = cursor.u64()?;
                let resume = (cursor.u8()? == 1).then_some(resume);
                let start = if cursor.u8()? == 1 {
                    let cwd = cursor.string()?;
                    let args = (0..cursor.count()?)
                        .map(|_| cursor.string())
                        .collect::<Result<_, _>>()?;
                    let env = (0..cursor.count()?)
                        .map(|_| Ok((cursor.string()?, cursor.string()?)))
                        .collect::<Result<_, WireError>>()?;
                    Some(Start { cwd, args, env })
                } else {
                    None
                };
                Self::Open(Open {
                    version,
                    size,
                    resume,
                    start,
                })
            }
            INPUT => return Ok(Self::Input(body.to_vec())),
            RESIZE => Self::Resize(cursor.size()?),
            other => return Err(WireError::UnknownKind(other)),
        };
        cursor.end()?;
        Ok(frame)
    }
}

impl FromHost {
    fn encode(&self) -> (u8, Vec<u8>) {
        let mut body = Vec::new();
        let kind = match self {
            Self::Ready(ready) => {
                body.push(match ready.state {
                    AttachState::Started => 0,
                    AttachState::Attached => 1,
                });
                put_u32(
                    &mut body,
                    usize::try_from(ready.pid.unwrap_or_default()).unwrap_or_default(),
                );
                put_u64(&mut body, ready.started_at);
                put_str(&mut body, &ready.cwd);
                put_u64(&mut body, ready.session);
                READY
            }
            Self::Output(bytes) => {
                body.extend_from_slice(bytes);
                OUTPUT
            }
            Self::Exit(code) => {
                body.extend_from_slice(&code.to_be_bytes());
                EXIT
            }
            Self::Replaced => REPLACED,
            Self::Detach => DETACH,
            Self::Failed(reason) => {
                put_str(&mut body, reason);
                FAILED
            }
        };
        (kind, body)
    }

    fn decode(kind: u8, body: &[u8]) -> Result<Self, WireError> {
        let mut cursor = Cursor { body, at: 0 };
        let frame = match kind {
            READY => {
                let state = match cursor.u8()? {
                    0 => AttachState::Started,
                    _ => AttachState::Attached,
                };
                let pid = cursor.u32()?;
                Self::Ready(Ready {
                    state,
                    pid: (pid > 0).then_some(pid),
                    started_at: cursor.u64()?,
                    cwd: cursor.string()?,
                    session: cursor.u64()?,
                })
            }
            OUTPUT => return Ok(Self::Output(body.to_vec())),
            EXIT => Self::Exit(i32::from_be_bytes(cursor.array()?)),
            REPLACED => Self::Replaced,
            DETACH => Self::Detach,
            FAILED => Self::Failed(cursor.string()?),
            other => return Err(WireError::UnknownKind(other)),
        };
        cursor.end()?;
        Ok(frame)
    }
}

pub async fn read_to_host<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Option<ToHost>, WireError> {
    match read_raw(reader).await? {
        Some((kind, body)) => ToHost::decode(kind, &body).map(Some),
        None => Ok(None),
    }
}

pub async fn read_from_host<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Option<FromHost>, WireError> {
    match read_raw(reader).await? {
        Some((kind, body)) => FromHost::decode(kind, &body).map(Some),
        None => Ok(None),
    }
}

pub async fn write_to_host<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &ToHost,
) -> Result<(), WireError> {
    let (kind, body) = frame.encode();
    write_raw(writer, kind, &body).await
}

pub async fn write_from_host<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &FromHost,
) -> Result<(), WireError> {
    let (kind, body) = frame.encode();
    write_raw(writer, kind, &body).await
}

async fn read_raw<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Option<(u8, Vec<u8>)>, WireError> {
    let mut header = [0u8; 5];
    match reader.read_exact(&mut header[..1]).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    reader.read_exact(&mut header[1..]).await?;
    let [kind, length @ ..] = header;
    let length = usize::try_from(u32::from_be_bytes(length)).unwrap_or(usize::MAX);
    if length > MAX_LINK_PAYLOAD {
        return Err(WireError::TooLarge(length));
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await?;
    Ok(Some((kind, body)))
}

async fn write_raw<W: AsyncWrite + Unpin>(
    writer: &mut W,
    kind: u8,
    body: &[u8],
) -> Result<(), WireError> {
    if body.len() > MAX_LINK_PAYLOAD {
        return Err(WireError::TooLarge(body.len()));
    }
    let mut frame = Vec::with_capacity(body.len() + 5);
    frame.push(kind);
    put_u32(&mut frame, body.len());
    frame.extend_from_slice(body);
    writer.write_all(&frame).await?;
    writer.flush().await?;
    Ok(())
}

fn put_u16(body: &mut Vec<u8>, value: u16) {
    body.extend_from_slice(&value.to_be_bytes());
}

fn put_u32(body: &mut Vec<u8>, value: usize) {
    body.extend_from_slice(&u32::try_from(value).unwrap_or(u32::MAX).to_be_bytes());
}

fn put_u64(body: &mut Vec<u8>, value: u64) {
    body.extend_from_slice(&value.to_be_bytes());
}

fn put_size(body: &mut Vec<u8>, size: Size) {
    put_u16(body, size.rows);
    put_u16(body, size.cols);
}

fn put_str(body: &mut Vec<u8>, value: &str) {
    put_u32(body, value.len());
    body.extend_from_slice(value.as_bytes());
}

struct Cursor<'a> {
    body: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8], WireError> {
        let end = self
            .at
            .checked_add(count)
            .filter(|end| *end <= self.body.len())
            .ok_or(WireError::Malformed("short link frame"))?;
        let slice = &self.body[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], WireError> {
        self.take(N)?
            .try_into()
            .map_err(|_| WireError::Malformed("short link frame"))
    }

    fn u8(&mut self) -> Result<u8, WireError> {
        Ok(u8::from_be_bytes(self.array()?))
    }

    fn u16(&mut self) -> Result<u16, WireError> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, WireError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, WireError> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    fn count(&mut self) -> Result<usize, WireError> {
        usize::try_from(self.u32()?).map_err(|_| WireError::Malformed("link count"))
    }

    fn size(&mut self) -> Result<Size, WireError> {
        Ok(Size {
            rows: self.u16()?,
            cols: self.u16()?,
        })
    }

    fn string(&mut self) -> Result<String, WireError> {
        let length = self.count()?;
        String::from_utf8(self.take(length)?.to_vec())
            .map_err(|_| WireError::Malformed("link text is not UTF-8"))
    }

    fn end(&self) -> Result<(), WireError> {
        if self.at == self.body.len() {
            Ok(())
        } else {
            Err(WireError::Malformed("trailing bytes in link frame"))
        }
    }
}
