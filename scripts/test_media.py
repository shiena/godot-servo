"""Run the Godot media self-check against a loopback server with byte ranges."""
import http.server
import os
from pathlib import Path
import re
import subprocess
import sys
import threading

ROOT = Path(__file__).resolve().parents[1]
VIDEO = (ROOT / "demo/web/media/platform-test.mp4").read_bytes()


class Media(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/playing":
            if self.server.downloading.is_set():
                self.server.progressive.set()
            self.server.playing.set()
            self.send_response(204)
            self.send_header("Access-Control-Allow-Origin", "*")
            self.end_headers()
            return
        if self.path != "/video.mp4":
            self.send_error(404)
            return
        start, end = 0, len(VIDEO) - 1
        requested = self.headers.get("Range")
        if requested:
            match = re.fullmatch(r"bytes=(\d+)-(\d*)", requested)
            if not match:
                self.send_error(416)
                return
            start = int(match[1])
            if match[2]:
                end = min(end, int(match[2]))
            if start > end:
                self.send_error(416)
                return
        self.send_response(206 if requested else 200)
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Type", "video/mp4")
        self.send_header("Access-Control-Allow-Origin", "*")
        if requested:
            self.send_header("Content-Range", f"bytes {start}-{end}/{len(VIDEO)}")
        self.send_header("Content-Length", str(end - start + 1))
        self.end_headers()
        cutoff = len(VIDEO) * 3 // 4
        if start == 0 and end >= cutoff and not self.server.playing.is_set():
            self.server.downloading.set()
            try:
                self.wfile.write(VIDEO[:cutoff])
                self.wfile.flush()
                self.server.playing.wait(10)
            finally:
                self.server.downloading.clear()
            start = cutoff
        try:
            self.wfile.write(VIDEO[start:end + 1])
        except (BrokenPipeError, ConnectionResetError):
            pass


if __name__ == "__main__":
    with http.server.ThreadingHTTPServer(("127.0.0.1", 0), Media) as server:
        server.downloading = threading.Event()
        server.playing = threading.Event()
        server.progressive = threading.Event()
        threading.Thread(target=server.serve_forever, daemon=True).start()
        url = f"http://127.0.0.1:{server.server_port}/video.mp4"
        result = subprocess.run([*sys.argv[1:], "--path", str(ROOT), "res://demo/audiotest.tscn"],
                                env={**os.environ, "GODOT_SERVO_TEST_MEDIA": url}, timeout=100)
        server.shutdown()
    print("Progressive playback:", "PASS" if server.progressive.is_set() else "FAIL")
    sys.exit(result.returncode or int(not server.progressive.is_set()))
