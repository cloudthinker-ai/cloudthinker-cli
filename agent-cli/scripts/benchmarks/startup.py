import argparse
import codecs
import fcntl
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import pty
import secrets
import select
import signal
import struct
import subprocess
import tempfile
import termios
import threading
import time

import pyte


QUERIES = {
    "\x1b[?996n": "\x1b[?997;1n",
    "\x1b[6n": "\x1b[1;1R", "\x1b[?6n": "\x1b[?1;1R",
    "\x1b[c": "\x1b[?1;2c", "\x1b[0c": "\x1b[?1;2c",
    "\x1b[>c": "\x1b[>0;276;0c", "\x1b[>0c": "\x1b[>0;276;0c",
    "\x1b[?u": "\x1b[?0u", "\x1b[?2026$p": "\x1b[?2026;2$y",
    "\x1b[14t": "\x1b[4;640;960t", "\x1b[16t": "\x1b[6;16;8t",
    "\x1b[18t": "\x1b[8;40;120t",
}
for terminator in ("\x07", "\x1b\\"):
    for code, color in [(10, "eeee/eeee/eeee"), (11, "0000/0000/0000")]:
        QUERIES[f"\x1b]{code};?{terminator}"] = f"\x1b]{code};rgb:{color}\x1b\\"


class Display:
    def __init__(self, reply):
        self.screen = pyte.Screen(120, 40)
        self.stream = pyte.Stream(self.screen)
        self.reply = reply
        self.pending = ""

    def feed(self, chunk):
        self.pending += chunk
        while True:
            matches = [(self.pending.find(query), query) for query in QUERIES if query in self.pending]
            if not matches:
                break
            position, query = min(matches)
            self.stream.feed(self.pending[:position])
            self.reply(QUERIES[query])
            self.pending = self.pending[position + len(query):]
        keep = max((length for query in QUERIES for length in range(1, len(query)) if self.pending.endswith(query[:length])), default=0)
        self.stream.feed(self.pending[:-keep] if keep else self.pending)
        self.pending = self.pending[-keep:] if keep else ""

    def text(self):
        return "\n".join(self.screen.display)


class FixtureAPI(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        self.respond()

    def do_POST(self):
        self.respond()

    def do_PUT(self):
        self.respond()

    def respond(self):
        self.rfile.read(int(self.headers.get("Content-Length", "0")))
        workspace = "11111111-1111-4111-8111-111111111111"
        conversation = "22222222-2222-4222-8222-222222222222"
        routes = {
            "/models": {"models": [{"id": "pro", "name": "Pro", "reasoning": True, "contextWindow": 200000, "maxTokens": 32000, "input": ["text"]}]},
            "/whoami": {"user_email": "fixture@example.invalid", "workspace_id": workspace, "workspace_name": "Startup fixture", "organization_id": None},
            "/sessions": {"conversation_id": conversation, "workspace_id": workspace, "web_url": "https://example.invalid/session", "auto_mode": {"enabled": True, "can_edit": True}},
            "/entries": {"entries": [], "last_seq": 0, "stored": 0},
            "/credits": {"credits_used": 0, "tokens_consumed": 0},
            "/connections": {"xml": "", "prefixes": []},
            "/custom-skills/": [],
            "/executions": {"status": "completed", "return_code": 0, "stdout": "", "stderr": ""},
        }
        matched = [body for suffix, body in routes.items() if self.path.split("?")[0].endswith(suffix)]
        self.send_response(200 if matched else 404)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps(matched[0] if matched else {"detail": "fixture route not found"}).encode())


def measure(binary, url, timeout=20):
    with tempfile.TemporaryDirectory(prefix="ct-startup-trial-") as directory:
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
        env = {"PATH": os.environ["PATH"], "HOME": directory, "XDG_CONFIG_HOME": directory,
               "TERM": "xterm-256color", "COLORTERM": "truecolor", "LANG": "C.UTF-8",
               "CLOUDTHINKER_CODING_AGENT_DIR": directory, "PI_CODING_AGENT_DIR": directory,
               "CLOUDTHINKER_TOKEN": "fixture-only", "CLOUDTHINKER_URL": url,
               "PI_SKIP_VERSION_CHECK": "1"}
        started = time.perf_counter()
        child = subprocess.Popen([str(binary), "--approve", "--no-extensions"], cwd=directory,
                                 env=env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        os.set_blocking(master, False)
        pending = bytearray()

        def send(text):
            pending.extend(text.encode())
            if len(pending) > 65536:
                raise RuntimeError("terminal input exceeded 64 KiB")

        display = Display(send)
        decoder = codecs.getincrementaldecoder("utf-8")("replace")
        probes = []
        origin = None
        echoed = None
        next_probe = 0
        transcript = ""
        try:
            while time.perf_counter() - started < timeout:
                if pending:
                    try:
                        written = os.write(master, pending)
                        del pending[:written]
                    except BlockingIOError:
                        pass
                if child.poll() is not None:
                    raise RuntimeError(f"agent exited before editor readiness: {child.returncode}\n{display.text()}")
                readable, _, _ = select.select([master], [], [], 0.005)
                updated = bool(readable)
                if updated:
                    chunk = decoder.decode(os.read(master, 65536))
                    transcript += chunk
                    if len(transcript) > 8_000_000:
                        raise RuntimeError("terminal output exceeded 8 MB")
                    display.feed(chunk)
                updated = updated and (2026 << 5) not in display.screen.mode
                if termios.tcgetattr(master)[3] & (termios.ICANON | termios.ECHO):
                    continue
                now = time.perf_counter()
                if origin is not None:
                    cursor = display.screen.cursor
                    if updated and not pending and (cursor.y, cursor.x) == origin and not any(probe in display.text() for probe in probes):
                        return {"ready_ms": (echoed - started) * 1000, "cleared_ms": (now - started) * 1000, "correct": True}
                    continue
                if updated and probes:
                    if echoed is None and any(probe in display.text() for probe in probes):
                        echoed = now
                    if echoed is not None:
                        for row, line in enumerate(display.screen.display):
                            column = line.find(probes[-1])
                            if column >= 0:
                                origin = (row, column)
                                send("\x7f" * 10)
                                break
                if echoed is None and not pending and now >= next_probe:
                    if probes:
                        send("\x7f" * 10)
                    probe = "b" + secrets.token_hex(4) + "r"
                    send(probe)
                    probes.append(probe)
                    next_probe = now + 0.02
            raise TimeoutError(f"editor readiness or exact probe cleanup not observed: origin={origin}, cursor={(display.screen.cursor.y, display.screen.cursor.x)}, probes={probes[-3:]}\n{display.text()}")
        finally:
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGTERM)
                try:
                    child.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait()
            os.close(master)


def run(baseline, candidate, count):
    if count < 3:
        raise ValueError("at least three paired trials are required")
    hashes = {}
    for label, binary in [("baseline", baseline), ("candidate", candidate)]:
        with binary.open("rb") as stream:
            magic = stream.read(4)
            if magic not in (b"\x7fELF", b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf", b"\xca\xfe\xba\xbe"):
                raise ValueError(f"{binary}: expected a compiled ELF or Mach-O binary")
            stream.seek(0)
            hashes[f"{label}_sha256"] = hashlib.file_digest(stream, "sha256").hexdigest()
    server = ThreadingHTTPServer(("127.0.0.1", 0), FixtureAPI)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    trials = {"baseline": [], "candidate": []}
    try:
        url = f"http://127.0.0.1:{server.server_port}"
        for index in range(count):
            order = [("baseline", baseline), ("candidate", candidate)]
            if index % 2:
                order.reverse()
            for label, binary in order:
                sample = {"index": index, **measure(binary, url)}
                trials[label].append(sample)
                print(f"{label} {index}: {sample['ready_ms']:.2f} ms", flush=True)
        return {"count": count, **hashes, "trials": trials}
    finally:
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("baseline", type=Path)
    parser.add_argument("candidate", type=Path)
    parser.add_argument("--count", type=int, default=10)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = run(args.baseline.resolve(strict=True), args.candidate.resolve(strict=True), args.count)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
