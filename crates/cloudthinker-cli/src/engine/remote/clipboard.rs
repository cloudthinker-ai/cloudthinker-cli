use std::process::Stdio;
use std::time::Duration;

#[cfg(not(target_os = "macos"))]
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::wire::MAX_UPLOAD;

const READ_TIMEOUT: Duration = Duration::from_secs(3);
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

pub async fn read_image() -> Option<Vec<u8>> {
    let png = tokio::time::timeout(READ_TIMEOUT, read_png())
        .await
        .ok()??;
    is_png(&png).then_some(png)
}

fn is_png(bytes: &[u8]) -> bool {
    bytes.len() <= MAX_UPLOAD && bytes.starts_with(PNG_SIGNATURE)
}

#[cfg(target_os = "macos")]
async fn read_png() -> Option<Vec<u8>> {
    let dir = tempfile::tempdir().ok()?;
    let path = dir.path().join("clipboard.png");
    let path_text = path.to_str()?;
    if path_text.contains(['"', '\\']) {
        return None;
    }
    let script = format!(
        "set png_data to (the clipboard as «class PNGf»)\nset fp to open for access POSIX file \"{path_text}\" with write permission\nwrite png_data to fp\nclose access fp"
    );
    let status = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status()
        .await
        .ok()?;
    if !status.success() {
        return None;
    }
    let len = tokio::fs::metadata(&path).await.ok()?.len();
    if !usize::try_from(len).is_ok_and(|len| len <= MAX_UPLOAD) {
        return None;
    }
    tokio::fs::read(&path).await.ok()
}

#[cfg(not(target_os = "macos"))]
async fn read_png() -> Option<Vec<u8>> {
    match wayland_png().await {
        Some(png) => Some(png),
        None => x11_png().await,
    }
}

#[cfg(not(target_os = "macos"))]
async fn wayland_png() -> Option<Vec<u8>> {
    let types = output("wl-paste", &["--list-types"]).await?;
    if !lists_png(&types) {
        return None;
    }
    output("wl-paste", &["--no-newline", "--type", "image/png"]).await
}

#[cfg(not(target_os = "macos"))]
async fn x11_png() -> Option<Vec<u8>> {
    let targets = output("xclip", &["-selection", "clipboard", "-t", "TARGETS", "-o"]).await?;
    if !lists_png(&targets) {
        return None;
    }
    output(
        "xclip",
        &["-selection", "clipboard", "-t", "image/png", "-o"],
    )
    .await
}

#[cfg(not(target_os = "macos"))]
async fn output(program: &str, args: &[&str]) -> Option<Vec<u8>> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let limit = u64::try_from(MAX_UPLOAD).ok()?.checked_add(1)?;
    let mut bytes = Vec::new();
    child
        .stdout
        .take()?
        .take(limit)
        .read_to_end(&mut bytes)
        .await
        .ok()?;
    if bytes.len() > MAX_UPLOAD {
        return None;
    }
    child.wait().await.ok()?.success().then_some(bytes)
}

#[cfg_attr(
    target_os = "macos",
    allow(dead_code, reason = "only the Linux clipboard tools list types")
)]
fn lists_png(types: &[u8]) -> bool {
    String::from_utf8_lossy(types)
        .lines()
        .any(|line| line.trim() == "image/png")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_png_under_the_cap_is_an_image() {
        assert!(is_png(b"\x89PNG\r\n\x1a\nrest"));
        assert!(!is_png(b"GIF89a"));
        assert!(!is_png(b""));
    }

    #[test]
    fn png_is_found_in_a_type_list() {
        assert!(lists_png(b"text/plain\nimage/png\n"));
        assert!(lists_png(b"TARGETS\r\nimage/png\r\n"));
        assert!(!lists_png(b"text/plain\nimage/jpeg\n"));
    }

    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn a_clipboard_tool_is_read_only_up_to_the_cap() {
        let at_cap = MAX_UPLOAD.to_string();
        let over_cap = (MAX_UPLOAD + 1).to_string();
        let read = output("head", &["-c", &at_cap, "/dev/zero"]).await;
        assert_eq!(read.map(|bytes| bytes.len()), Some(MAX_UPLOAD));
        assert_eq!(output("head", &["-c", &over_cap, "/dev/zero"]).await, None);
        assert_eq!(output("false", &[]).await, None);
    }
}
