#!/usr/bin/env python3

from __future__ import annotations

import argparse
import json
import math
import subprocess
import threading
import time
from dataclasses import dataclass
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse


ROOT_DIR = Path(__file__).resolve().parents[1]
STATION_ASSETS_DIR = ROOT_DIR / "assets" / "station"


def read_text(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def read_bytes(path: Path) -> bytes:
    return path.read_bytes()


def detect_build_id() -> str:
    try:
        git_short = (
            subprocess.run(
                ["git", "rev-parse", "--short", "HEAD"],
                cwd=ROOT_DIR,
                check=True,
                capture_output=True,
                text=True,
            )
            .stdout.strip()
        )
    except (FileNotFoundError, subprocess.CalledProcessError):
        git_short = "nogit"
    return f"{git_short}-mock"


@dataclass(frozen=True)
class MockScaleDevice:
    address: str
    name: str
    address_type: str
    rssi: int
    protocol_hint: str


@dataclass
class SavedScale:
    address: str
    name: str
    addr_type: str

    @classmethod
    def from_device(cls, device: MockScaleDevice) -> "SavedScale":
        return cls(
            address=device.address,
            name=device.name,
            addr_type=device.address_type,
        )


class MockUiState:
    def __init__(self, public_host: str, port: int, build_id: str, board_id: str) -> None:
        self.public_host = public_host
        self.port = port
        self.build_id = build_id
        self.board_id = board_id
        self.started_at = time.monotonic()
        self.seq = 0
        self.lock = threading.Lock()

        self.settings = {
            "ssid": "OpenBaristaLab",
            "device_label": "OpenBarista Mock",
            "temperature_offset_c": 0.0,
        }
        self.networks = [
            "OpenBaristaLab",
            "Cafe Bench",
            "Barista Test",
            "Lab Guest",
        ]

        self.devices = [
            MockScaleDevice(
                address="AA:BB:CC:01:02:03",
                name="Acaia Lunar Mock",
                address_type="public",
                rssi=-47,
                protocol_hint="acaia",
            ),
            MockScaleDevice(
                address="AA:BB:CC:04:05:06",
                name="Bookoo Mock",
                address_type="public",
                rssi=-61,
                protocol_hint="ble-weight",
            ),
        ]

        default_scale = SavedScale.from_device(self.devices[0])
        self.saved_scale: SavedScale | None = default_scale
        self.connected_scale: SavedScale | None = default_scale
        self.scale_state = "ready"
        self.scale_message = "Mock scale streaming live data."
        self.scan_until = 0.0

    @property
    def direct_ip(self) -> str:
        return f"{self.public_host}:{self.port}"

    def settings_payload(
        self,
        *,
        ok: bool = True,
        message: str = "ok",
        rebooting: bool = False,
    ) -> dict[str, object]:
        return {
            "ok": ok,
            "message": message,
            "rebooting": rebooting,
            "ssid": self.settings["ssid"],
            "device_label": self.settings["device_label"],
            "temperature_offset_c": self.settings["temperature_offset_c"],
            "ip_addr": self.direct_ip,
            "build_id": self.build_id,
            "board_id": self.board_id,
        }

    def update_settings(self, form: dict[str, str]) -> tuple[int, dict[str, object]]:
        with self.lock:
            wifi_update = form.get("wifi_update", "") in {"1", "true", "on"}
            device_label = form.get("device_label", "").strip() or "OpenBarista"
            offset_raw = form.get("temperature_offset_c", "0").strip() or "0"

            try:
                temperature_offset_c = round(float(offset_raw), 1)
            except ValueError:
                return HTTPStatus.BAD_REQUEST, self.settings_payload(
                    ok=False,
                    message="Temperature offset must be a valid number.",
                )

            self.settings["device_label"] = device_label
            self.settings["temperature_offset_c"] = temperature_offset_c

            if wifi_update:
                requested_ssid = form.get("ssid", "").strip()
                if requested_ssid:
                    self.settings["ssid"] = requested_ssid
                message = "Wi-Fi settings saved in mock mode."
            else:
                message = "Device settings saved in mock mode."

            return HTTPStatus.OK, self.settings_payload(
                ok=True,
                message=message,
                rebooting=False,
            )

    def scale_status_payload(self) -> dict[str, object]:
        with self.lock:
            now = time.monotonic()
            if self.scale_state == "scanning" and now >= self.scan_until:
                self.scale_state = "idle"
                self.scale_message = "Mock scan complete."

            connected = self.connected_scale
            telemetry = self._telemetry_sample(now)
            saved_address = self.saved_scale.address if self.saved_scale else None

            return {
                "available": True,
                "state": self.scale_state,
                "message": self.scale_message,
                "connected_name": connected.name if connected else "",
                "connected_address": connected.address if connected else "",
                "protocol": "mock-ble" if connected else "",
                "weight_g": telemetry["weight_g"] if connected else 0.0,
                "flow_gps": telemetry["flow_gps"] if connected else 0.0,
                "battery_percent": 87 if connected else None,
                "saved_scale": (
                    {
                        "address": self.saved_scale.address,
                        "name": self.saved_scale.name,
                        "addr_type": self.saved_scale.addr_type,
                    }
                    if self.saved_scale
                    else None
                ),
                "devices": [
                    {
                        "address": device.address,
                        "name": device.name,
                        "address_type": device.address_type,
                        "rssi": device.rssi,
                        "protocol_hint": device.protocol_hint,
                        "saved": saved_address == device.address,
                    }
                    for device in self.devices
                ],
            }

    def handle_scale_action(self, form: dict[str, str]) -> tuple[int, dict[str, object]]:
        action = form.get("action", "").strip()
        address = form.get("address", "").strip().lower()

        with self.lock:
            if action == "scan":
                self.scale_state = "scanning"
                self.scale_message = "Finding nearby mock scales..."
                self.scan_until = time.monotonic() + 2.0
                return HTTPStatus.OK, {"ok": True, "message": self.scale_message}

            if action == "connect":
                target = next(
                    (
                        device
                        for device in self.devices
                        if device.address.lower() == address
                    ),
                    None,
                )

                if target is None and self.saved_scale is not None:
                    if not address or self.saved_scale.address.lower() == address:
                        self.connected_scale = self.saved_scale
                        self.scale_state = "ready"
                        self.scale_message = f"Connected to {self.saved_scale.name}."
                        return HTTPStatus.OK, {"ok": True, "message": self.scale_message}

                if target is None:
                    return HTTPStatus.BAD_REQUEST, {
                        "ok": False,
                        "message": "Scan first, then tap a device from the list.",
                    }

                self.saved_scale = SavedScale.from_device(target)
                self.connected_scale = self.saved_scale
                self.scale_state = "ready"
                self.scale_message = f"Connected to {target.name}."
                return HTTPStatus.OK, {"ok": True, "message": self.scale_message}

            if action == "disconnect":
                self.connected_scale = None
                self.scale_state = "idle"
                self.scale_message = "Mock scale disconnected."
                return HTTPStatus.OK, {"ok": True, "message": self.scale_message}

            if action == "forget":
                self.connected_scale = None
                self.saved_scale = None
                self.scale_state = "idle"
                self.scale_message = "Saved mock scale forgotten."
                return HTTPStatus.OK, {"ok": True, "message": self.scale_message}

            return HTTPStatus.BAD_REQUEST, {
                "ok": False,
                "message": "Unsupported scale action.",
            }

    def telemetry_payload(self) -> dict[str, object]:
        with self.lock:
            self.seq += 1
            sample = self._telemetry_sample(time.monotonic())
            return {
                "seq": self.seq,
                "temperature_c": sample["temperature_c"],
                "pressure_bar": sample["pressure_bar"],
                "pressure_psi": sample["pressure_psi"],
                "scale_connected": self.connected_scale is not None,
                "weight_g": sample["weight_g"] if self.connected_scale else 0.0,
                "flow_gps": sample["flow_gps"] if self.connected_scale else 0.0,
            }

    def _telemetry_sample(self, now: float) -> dict[str, float]:
        elapsed = now - self.started_at
        cycle_t = elapsed % 40.0

        if cycle_t < 4.0:
            pressure_bar = cycle_t * 0.8
        elif cycle_t < 10.0:
            pressure_bar = 3.2 + (cycle_t - 4.0) * 0.95
        elif cycle_t < 28.0:
            pressure_bar = 8.9 + 0.35 * math.sin(cycle_t * 0.8)
        elif cycle_t < 34.0:
            pressure_bar = max(0.0, 8.5 - (cycle_t - 28.0) * 1.3)
        else:
            pressure_bar = 0.0

        flow_gps = 0.0
        brew_weight_g = 0.0
        if cycle_t >= 5.0:
            active_t = min(cycle_t - 5.0, 25.0)
            if active_t > 0:
                flow_gps = max(0.0, 2.8 - 0.05 * active_t + 0.18 * math.sin(active_t * 1.1))
                brew_weight_g = max(0.0, active_t * 1.6 + 0.9 * math.sin(active_t * 0.55))

        temperature_c = (
            93.2
            + self.settings["temperature_offset_c"]
            + 0.45 * math.sin(elapsed / 16.0)
            + 0.18 * math.sin(elapsed / 3.8)
        )
        weight_g = 132.0 + brew_weight_g

        return {
            "temperature_c": round(temperature_c, 3),
            "pressure_bar": round(pressure_bar, 3),
            "pressure_psi": round(pressure_bar * 14.5038, 3),
            "weight_g": round(weight_g, 3),
            "flow_gps": round(flow_gps, 3),
        }


def render_settings_html(state: MockUiState) -> bytes:
    html = read_text(STATION_ASSETS_DIR / "settings.html")
    html = html.replace("{{BUILD_ID}}", state.build_id)
    html = html.replace("{{BOARD_ID}}", state.board_id)
    html = html.replace("{{IP_ADDR}}", state.direct_ip)
    return html.encode("utf-8")


class HeadlessUiHandler(BaseHTTPRequestHandler):
    server_version = "OpenBaristaMock/1.0"

    def do_GET(self) -> None:
        parsed = urlparse(self.path)
        path = parsed.path
        state: MockUiState = self.server.state  # type: ignore[attr-defined]

        if path == "/":
            self._send_bytes(HTTPStatus.OK, "text/html; charset=utf-8", read_bytes(STATION_ASSETS_DIR / "index.html"))
            return
        if path == "/settings":
            self._send_bytes(HTTPStatus.OK, "text/html; charset=utf-8", render_settings_html(state))
            return
        if path == "/health":
            self._send_bytes(HTTPStatus.OK, "text/plain; charset=utf-8", b"ok")
            return
        if path == "/networks":
            self._send_json(HTTPStatus.OK, state.networks)
            return
        if path == "/api/telemetry":
            self._send_json(HTTPStatus.OK, state.telemetry_payload())
            return
        if path == "/api/settings":
            self._send_json(HTTPStatus.OK, state.settings_payload())
            return
        if path == "/api/scale":
            self._send_json(HTTPStatus.OK, state.scale_status_payload())
            return

        static_asset = {
            "/base.css": ("text/css; charset=utf-8", STATION_ASSETS_DIR / "base.css"),
            "/station.css": ("text/css; charset=utf-8", STATION_ASSETS_DIR / "station.css"),
            "/station.js": ("application/javascript; charset=utf-8", STATION_ASSETS_DIR / "station.js"),
            "/settings.css": ("text/css; charset=utf-8", STATION_ASSETS_DIR / "settings.css"),
            "/settings.js": ("application/javascript; charset=utf-8", STATION_ASSETS_DIR / "settings.js"),
            "/uplot.min.css": ("text/css; charset=utf-8", STATION_ASSETS_DIR / "uplot.min.css"),
            "/uplot.min.js": ("application/javascript; charset=utf-8", STATION_ASSETS_DIR / "uplot.min.js"),
        }.get(path)

        if static_asset is None:
            self._send_json(HTTPStatus.NOT_FOUND, {"ok": False, "message": "Not found."})
            return

        content_type, asset_path = static_asset
        self._send_bytes(HTTPStatus.OK, content_type, read_bytes(asset_path))

    def do_POST(self) -> None:
        parsed = urlparse(self.path)
        path = parsed.path
        state: MockUiState = self.server.state  # type: ignore[attr-defined]
        form = self._read_form()
        if form is None:
            return

        if path == "/api/settings":
            status, payload = state.update_settings(form)
            self._send_json(status, payload)
            return
        if path == "/api/scale":
            status, payload = state.handle_scale_action(form)
            self._send_json(status, payload)
            return

        self._send_json(HTTPStatus.NOT_FOUND, {"ok": False, "message": "Not found."})

    def log_message(self, format: str, *args: object) -> None:
        return

    _MAX_BODY_BYTES = 512

    def _read_form(self) -> dict[str, str] | None:
        raw_cl = self.headers.get("Content-Length", "0")
        try:
            length = int(raw_cl)
        except (ValueError, TypeError):
            self._send_json(HTTPStatus.BAD_REQUEST, {"ok": False, "message": "Invalid Content-Length."})
            return None
        if length < 0 or length > self._MAX_BODY_BYTES:
            self._send_json(
                HTTPStatus.REQUEST_ENTITY_TOO_LARGE,
                {"ok": False, "message": f"Body too large (max {self._MAX_BODY_BYTES} bytes)."},
            )
            return None
        raw_body = self.rfile.read(length).decode("utf-8")
        parsed = parse_qs(raw_body, keep_blank_values=True)
        return {key: values[-1] for key, values in parsed.items()}

    def _send_json(self, status: int, payload: object) -> None:
        body = json.dumps(payload, separators=(",", ":")).encode("utf-8")
        self._send_bytes(status, "application/json; charset=utf-8", body)

    def _send_bytes(self, status: int, content_type: str, body: bytes) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Serve the OpenBarista station UI locally with mock data.",
    )
    parser.add_argument("--host", default="127.0.0.1", help="Bind address for the local server.")
    parser.add_argument("--port", type=int, default=4173, help="Port for the local server.")
    parser.add_argument(
        "--public-host",
        default="127.0.0.1",
        help="Host name written into the settings page direct-link fields.",
    )
    parser.add_argument("--build-id", default=detect_build_id(), help="Build ID shown in the UI.")
    parser.add_argument("--board-id", default="MOCK-UI", help="Board ID shown in the UI.")
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    state = MockUiState(
        public_host=args.public_host,
        port=args.port,
        build_id=args.build_id,
        board_id=args.board_id,
    )
    server = ThreadingHTTPServer((args.host, args.port), HeadlessUiHandler)
    server.state = state  # type: ignore[attr-defined]

    print(f"OpenBarista mock UI running at http://{args.public_host}:{args.port}")
    print("Press Ctrl+C to stop.")

    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
