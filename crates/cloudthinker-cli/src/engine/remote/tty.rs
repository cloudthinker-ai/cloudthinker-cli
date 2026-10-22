use std::io::{self, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::termios::{OptionalActions, Termios, tcgetattr, tcgetwinsize, tcsetattr};
use tokio::io::unix::AsyncFd;

use super::wire::Size;

pub const RESET: &[u8] = b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[<u\x1b[>4;0m\x1b[?25h\x1b[?7h\x1b[0m";
const ENTER_ALT_SCREEN: &[u8] = b"\x1b[?1049h";
const LEAVE_ALT_SCREEN: &[u8] = b"\x1b[?1049l";
const PROMPT_RESET: &[u8] = b"\x1b[?25h\x1b[0m";
const READ_CHUNK: usize = 16 * 1024;

pub struct RawMode {
    fd: OwnedFd,
    termios: Termios,
    flags: OFlags,
    reset: &'static [u8],
    alt_screen: std::cell::Cell<bool>,
}

impl RawMode {
    fn enter(reset: &'static [u8]) -> io::Result<Self> {
        let fd = rustix::io::dup(io::stdin().as_fd())?;
        let termios = tcgetattr(&fd)?;
        let flags = fcntl_getfl(&fd)?;
        let mut raw = termios.clone();
        raw.make_raw();
        tcsetattr(&fd, OptionalActions::Now, &raw)?;
        let mode = Self {
            fd,
            termios,
            flags,
            reset,
            alt_screen: std::cell::Cell::new(false),
        };
        fcntl_setfl(&mode.fd, flags | OFlags::NONBLOCK)?;
        Ok(mode)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = tcsetattr(&self.fd, OptionalActions::Now, &self.termios);
        let _ = fcntl_setfl(&self.fd, self.flags);
        let mut stdout = io::stdout();
        if self.alt_screen.get() {
            let _ = stdout.write_all(LEAVE_ALT_SCREEN);
        }
        let _ = stdout.write_all(self.reset);
        let _ = stdout.flush();
    }
}

pub struct RawTerminal {
    pub input: TtyInput,
    pub output: TtyOutput,
    _mode: RawMode,
}

impl RawTerminal {
    pub fn enter() -> io::Result<Self> {
        Self::with_reset(RESET)
    }

    pub fn prompt() -> io::Result<Self> {
        Self::with_reset(PROMPT_RESET)
    }

    pub fn track(&self, output: &[u8]) {
        if let Some(on) = alt_screen_after(output) {
            self._mode.alt_screen.set(on);
        }
    }

    fn with_reset(reset: &'static [u8]) -> io::Result<Self> {
        let mode = RawMode::enter(reset)?;
        Ok(Self {
            input: TtyInput::new()?,
            output: TtyOutput::new()?,
            _mode: mode,
        })
    }
}

pub struct TtyInput {
    fd: AsyncFd<OwnedFd>,
}

impl TtyInput {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            fd: AsyncFd::new(rustix::io::dup(io::stdin().as_fd())?)?,
        })
    }

    pub async fn read(&self) -> io::Result<Vec<u8>> {
        loop {
            let mut guard = self.fd.readable().await?;
            let mut buf = vec![0u8; READ_CHUNK];
            match guard
                .try_io(|fd| rustix::io::read(fd.get_ref(), &mut buf[..]).map_err(io::Error::from))
            {
                Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => {}
                Ok(Ok(len)) => {
                    buf.truncate(len);
                    return Ok(buf);
                }
                Ok(Err(error)) => return Err(error),
                Err(_would_block) => {}
            }
        }
    }
}

pub struct TtyOutput {
    fd: AsyncFd<OwnedFd>,
}

impl TtyOutput {
    fn new() -> io::Result<Self> {
        Ok(Self {
            fd: AsyncFd::new(rustix::io::dup(io::stdout().as_fd())?)?,
        })
    }

    pub async fn write_all(&self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let mut guard = self.fd.writable().await?;
            match guard.try_io(|fd| rustix::io::write(fd.get_ref(), bytes).map_err(io::Error::from))
            {
                Ok(Ok(0)) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(Ok(written)) => bytes = bytes.get(written..).unwrap_or_default(),
                Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => {}
                Ok(Err(error)) => return Err(error),
                Err(_would_block) => {}
            }
        }
        Ok(())
    }
}

pub fn window_size() -> Option<Size> {
    size_of(io::stdout().as_fd()).or_else(|| size_of(io::stdin().as_fd()))
}

pub fn bottom_line(text: &str, size: Size) -> Vec<u8> {
    let text: String = text.chars().take(usize::from(size.cols)).collect();
    format!("\x1b7\x1b[{};1H\x1b[2K\x1b[7m{text}\x1b[0m\x1b8", size.rows).into_bytes()
}

pub fn clear_bottom_line(size: Size) -> Vec<u8> {
    format!("\x1b7\x1b[{};1H\x1b[2K\x1b8", size.rows).into_bytes()
}

fn size_of(fd: BorrowedFd<'_>) -> Option<Size> {
    let size = tcgetwinsize(fd).ok()?;
    nonzero_size(size.ws_row, size.ws_col)
}

fn alt_screen_after(output: &[u8]) -> Option<bool> {
    let last = |needle: &[u8]| {
        output
            .windows(needle.len())
            .rposition(|window| window == needle)
    };
    match (last(ENTER_ALT_SCREEN), last(LEAVE_ALT_SCREEN)) {
        (Some(enter), Some(leave)) => Some(enter > leave),
        (Some(_), None) => Some(true),
        (None, Some(_)) => Some(false),
        (None, None) => None,
    }
}

fn nonzero_size(rows: u16, cols: u16) -> Option<Size> {
    (rows > 0 && cols > 0).then_some(Size { rows, cols })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_turns_off_every_mode_the_agent_may_leave_on() {
        let reset = String::from_utf8_lossy(RESET);
        for mode in [
            "\x1b[?1000l",
            "\x1b[?1002l",
            "\x1b[?1003l",
            "\x1b[?1006l",
            "\x1b[?1004l",
            "\x1b[?2004l",
            "\x1b[<u",
            "\x1b[?25h",
            "\x1b[0m",
        ] {
            assert!(reset.contains(mode), "{mode:?}");
        }
    }

    #[test]
    fn only_a_screen_the_agent_left_open_is_closed_again() {
        assert_eq!(alt_screen_after(b"\x1b[?1049hdraw"), Some(true));
        assert_eq!(
            alt_screen_after(b"\x1b[?1049h..\x1b[?1049lbye"),
            Some(false)
        );
        assert_eq!(alt_screen_after(b"\x1b[?1049l..\x1b[?1049h"), Some(true));
        assert_eq!(alt_screen_after(b"plain text"), None);
    }

    #[test]
    fn the_banner_draws_on_the_last_row_and_puts_the_cursor_back() {
        let size = Size { rows: 30, cols: 8 };
        assert_eq!(
            bottom_line("lost link here", size),
            b"\x1b7\x1b[30;1H\x1b[2K\x1b[7mlost lin\x1b[0m\x1b8"
        );
        assert_eq!(clear_bottom_line(size), b"\x1b7\x1b[30;1H\x1b[2K\x1b8");
    }

    #[test]
    fn a_zero_size_is_never_reported() {
        assert_eq!(nonzero_size(0, 80), None);
        assert_eq!(nonzero_size(24, 0), None);
        assert_eq!(nonzero_size(24, 80), Some(Size { rows: 24, cols: 80 }));
    }
}
