use cloudthinker_client::{
    CLI_BINARY_NAME, CtError, CtResult, LinuxLibc, RELEASE_URL_ENV_VAR, ReleaseFile,
    fetch_host_release, linux_targets, release_base,
};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const RELEASES_DIR: &str = "$HOME/.cloudthinker/releases";

pub const PROBE: &str = "echo \"os=$(uname -s)\"; echo \"arch=$(uname -m)\"; \
echo \"ldd=$( (ldd --version 2>&1 || true) | head -n 1)\"; \
if [ -e /lib64/ld-linux-x86-64.so.2 ] || [ -e /lib/ld-linux-aarch64.so.1 ]; then echo glibc_loader=1; fi; \
if ls /lib/ld-musl-*.so.1 >/dev/null 2>&1; then echo musl_loader=1; fi";

#[derive(Debug, PartialEq, Eq)]
pub struct Probe {
    pub arch: String,
    pub libc: LinuxLibc,
}

pub fn parse_probe(output: &str) -> CtResult<Probe> {
    let mut os = None;
    let mut arch = None;
    let mut ldd = "";
    let mut libc = LinuxLibc {
        glibc: None,
        glibc_loader: false,
        musl_loader: false,
    };
    for line in output.lines() {
        match line.split_once('=') {
            Some(("os", value)) => os = Some(value.trim()),
            Some(("arch", value)) => arch = Some(value.trim()),
            Some(("ldd", value)) => ldd = value,
            Some(("glibc_loader", _)) => libc.glibc_loader = true,
            Some(("musl_loader", _)) => libc.musl_loader = true,
            _ => {}
        }
    }
    if os != Some("Linux") {
        return Err(CtError::Usage(format!(
            "cloudthinker can copy itself only to a Linux machine; the target reports {}",
            os.unwrap_or("no operating system")
        )));
    }
    if !ldd.contains("musl") {
        libc.glibc = ldd.split_whitespace().last().and_then(glibc_version);
    }
    let arch = arch
        .filter(|arch| !arch.is_empty())
        .ok_or_else(|| CtError::Usage("the target did not report its CPU type".into()))?;
    Ok(Probe {
        arch: arch.to_string(),
        libc,
    })
}

fn glibc_version(word: &str) -> Option<(u32, u32)> {
    let (major, minor) = word.split_once('.')?;
    let minor: String = minor.chars().take_while(char::is_ascii_digit).collect();
    Some((major.parse().ok()?, minor.parse().ok()?))
}

pub async fn release_for(probe: &Probe) -> CtResult<Vec<ReleaseFile>> {
    let targets = linux_targets(&probe.arch, probe.libc)?;
    fetch_host_release(&release_base()?, VERSION, targets).await
}

pub fn payload(files: &[ReleaseFile]) -> CtResult<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    for file in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(file.bytes.len() as u64);
        header.set_mode(file.mode);
        header.set_cksum();
        builder
            .append_data(&mut header, &file.name, file.bytes.as_slice())
            .map_err(|error| {
                CtError::Transport(format!("could not pack {}: {error}", file.name))
            })?;
    }
    builder
        .into_inner()
        .map_err(|error| CtError::Transport(format!("could not pack the release: {error}")))
}

pub fn install_script() -> String {
    format!(
        "set -e; r=\"{RELEASES_DIR}/v{VERSION}\"; mkdir -p \"$r\"; cd \"$r\"; tar -xf -; \
d=$(command -v {CLI_BINARY_NAME} 2>/dev/null || true); \
if [ -z \"$d\" ] || [ ! -w \"$d\" ]; then d=\"$HOME/.local/bin/{CLI_BINARY_NAME}\"; fi; \
mkdir -p \"$(dirname \"$d\")\"; mv -f {CLI_BINARY_NAME} \"$d\"; \"$d\" --version"
    )
}

pub fn release_env() -> String {
    format!(
        "v=$(\"$b\" --version 2>/dev/null); v=${{v##* }}; r=\"{RELEASES_DIR}\"; \
if [ -f \"$r/v$v/cloudthinker-agent-sha256.sum\" ]; then export {RELEASE_URL_ENV_VAR}=\"$r\"; fi"
    )
}

pub fn failure_message(target: &str, code: Option<i32>) -> String {
    match code {
        Some(code) => format!("copying cloudthinker to {target} failed with status {code}"),
        None => format!("copying cloudthinker to {target} stopped without an exit status"),
    }
}
