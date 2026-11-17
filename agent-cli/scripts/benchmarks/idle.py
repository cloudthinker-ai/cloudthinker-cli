import argparse
import codecs
import fcntl
import hashlib
import json
import os
import pty
import select
import signal
import statistics
import struct
import subprocess
import tempfile
import termios
import threading
import time
from dataclasses import asdict, dataclass
from http.server import ThreadingHTTPServer
from pathlib import Path

from startup import Display, FixtureAPI

CLOCK_TICKS = os.sysconf("SC_CLK_TCK")


@dataclass(frozen=True)
class Sample:
    switches: dict[str, int]
    cpu_seconds: float
    rss_kib: int
    threads: int


@dataclass(frozen=True)
class Trial:
    wakeups_per_second: float
    cpu_percent: float
    rss_mib: float
    threads: int
    repaint_bytes_per_second: float


def sample(pid: int) -> Sample:
    switches: dict[str, int] = {}
    for task in Path(f"/proc/{pid}/task").iterdir():
        try:
            status = (task / "status").read_text()
        except FileNotFoundError:
            continue
        switches[task.name] = sum(
            int(line.split()[1])
            for line in status.splitlines()
            if line.startswith(("voluntary_ctxt_switches:", "nonvoluntary_ctxt_switches:"))
        )
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    cpu_seconds = (int(fields[11]) + int(fields[12])) / CLOCK_TICKS
    rss_kib = next(
        int(line.split()[1])
        for line in Path(f"/proc/{pid}/status").read_text().splitlines()
        if line.startswith("VmRSS:")
    )
    return Sample(switches, cpu_seconds, rss_kib, len(switches))


def pump(master: int, display: Display, decoder: codecs.IncrementalDecoder, pending: bytearray, until: float) -> int:
    received = 0
    while time.perf_counter() < until:
        if pending:
            try:
                del pending[: os.write(master, pending)]
            except BlockingIOError:
                pass
        readable, _, _ = select.select([master], [], [], 0.05)
        if readable:
            chunk = os.read(master, 65536)
            received += len(chunk)
            display.feed(decoder.decode(chunk))
    return received


def measure(binary: Path, url: str, extra_env: dict[str, str], warmup: float, window: float) -> Trial:
    with tempfile.TemporaryDirectory(prefix="ct-idle-trial-") as directory:
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
        env = {"PATH": os.environ["PATH"], "HOME": directory, "XDG_CONFIG_HOME": directory,
               "TERM": "xterm-256color", "COLORTERM": "truecolor", "LANG": "C.UTF-8",
               "CLOUDTHINKER_CODING_AGENT_DIR": directory, "PI_CODING_AGENT_DIR": directory,
               "CLOUDTHINKER_TOKEN": "fixture-only", "CLOUDTHINKER_URL": url,
               "PI_SKIP_VERSION_CHECK": "1", **extra_env}
        child = subprocess.Popen([str(binary), "--approve", "--no-extensions"], cwd=directory,
                                 env=env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        os.set_blocking(master, False)
        pending = bytearray()

        def send(text: str) -> None:
            pending.extend(text.encode())

        display = Display(send)
        decoder = codecs.getincrementaldecoder("utf-8")("replace")
        try:
            pump(master, display, decoder, pending, time.perf_counter() + warmup)
            if child.poll() is not None:
                raise RuntimeError(f"agent exited during warmup: {child.returncode}\n{display.text()}")
            before = sample(child.pid)
            started = time.perf_counter()
            received = pump(master, display, decoder, pending, started + window)
            after = sample(child.pid)
            elapsed = time.perf_counter() - started
            if child.poll() is not None:
                raise RuntimeError(f"agent exited during the idle window: {child.returncode}")
            return Trial(
                wakeups_per_second=sum(
                    count - before.switches.get(tid, 0) for tid, count in after.switches.items()
                ) / elapsed,
                cpu_percent=100 * (after.cpu_seconds - before.cpu_seconds) / elapsed,
                rss_mib=after.rss_kib / 1024,
                threads=after.threads,
                repaint_bytes_per_second=received / elapsed,
            )
        finally:
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGTERM)
                try:
                    child.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait()
            os.close(master)


def parse_env(pairs: list[str]) -> dict[str, str]:
    env: dict[str, str] = {}
    for pair in pairs:
        key, separator, value = pair.partition("=")
        if not separator or not key:
            raise ValueError(f"--env expects KEY=VALUE, got {pair!r}")
        env[key] = value
    return env


def main() -> None:
    parser = argparse.ArgumentParser(description="Idle cost of one compiled agent binary at an empty prompt.")
    parser.add_argument("binary", type=Path)
    parser.add_argument("--env", action="append", default=[], help="KEY=VALUE for the agent, as the wrapper sets it")
    parser.add_argument("--count", type=int, default=5)
    parser.add_argument("--warmup", type=float, default=8.0)
    parser.add_argument("--window", type=float, default=10.0)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    with binary.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    server = ThreadingHTTPServer(("127.0.0.1", 0), FixtureAPI)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        url = f"http://127.0.0.1:{server.server_port}"
        trials = [measure(binary, url, parse_env(args.env), args.warmup, args.window) for _ in range(args.count)]
    finally:
        server.shutdown()
        server.server_close()
    medians = {field: statistics.median(getattr(trial, field) for trial in trials) for field in asdict(trials[0])}
    result = {"binary_sha256": digest, "env": parse_env(args.env), "window_seconds": args.window,
              "median": medians, "trials": [asdict(trial) for trial in trials]}
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(medians))


if __name__ == "__main__":
    main()
