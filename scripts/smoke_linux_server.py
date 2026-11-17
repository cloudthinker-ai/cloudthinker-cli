import http.server
import json
import sys
from functools import partial
from pathlib import Path

WHOAMI_PATH = "/api/v1/cli/whoami"
WHOAMI = json.dumps(
    {
        "user_id": "00000000-0000-4000-8000-000000000001",
        "user_email": "smoke@example.com",
        "workspace_id": "00000000-0000-4000-8000-000000000002",
        "workspace_name": "Smoke",
        "organization_id": None,
    }
).encode()


class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self) -> None:
        if self.path.split("?", 1)[0] != WHOAMI_PATH:
            super().do_GET()
            return
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(WHOAMI)))
        self.end_headers()
        self.wfile.write(WHOAMI)

    def log_message(self, format: str, *args: object) -> None:
        return


def main() -> None:
    directory = Path(sys.argv[1]).resolve()
    server = http.server.ThreadingHTTPServer(
        ("127.0.0.1", 0), partial(Handler, directory=str(directory))
    )
    print(server.server_address[1], flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
