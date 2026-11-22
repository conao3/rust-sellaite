import base64
import http.cookiejar
import json
import os
import struct
import subprocess
import urllib.parse
import urllib.request


def screenshot(env):
    with subprocess.Popen(
        ["sellaite", "mcp", "serve-mcp"],
        env=env,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
    ) as server:
        request_id = 0

        def request(method, params):
            nonlocal request_id
            request_id += 1
            server.stdin.write(json.dumps({
                "jsonrpc": "2.0", "id": request_id, "method": method, "params": params,
            }) + "\n")
            server.stdin.flush()
            response = json.loads(server.stdout.readline())
            assert response["id"] == request_id and "result" in response, response
            return response["result"]

        def tool(name, arguments, error=None):
            result = request("tools/call", {"name": name, "arguments": arguments})
            if error:
                assert result.get("isError") and error in result["content"][0]["text"], result
            else:
                assert not result.get("isError"), result
            return result["content"]

        try:
            request("initialize", {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "install-smoke", "version": "1"},
            })
            server.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
            tool("start_desktop", {"width": 640, "height": 480})
            tool("launch", {"argv": ["foot"], "wait_for_window": True})
            if env["SELLAITE_ATSPI_LIBRARY"] == "libatspi.so.0":
                tool("type", {"text": "hello"})
                image, = tool("screenshot", {})
                assert image["type"] == "image" and image["mimeType"] == "image/png"
                png = base64.b64decode(image["data"], validate=True)
                assert png[:8] == b"\x89PNG\r\n\x1a\n"
                assert struct.unpack(">II", png[16:24]) == (640, 480)
                print(f"MCP screenshot: 640x480 PNG, {len(png)} bytes", flush=True)
            else:
                error, = tool("type", {"text": "hello"}, error="cannot load libatspi")
                assert "libatspi2.0-0t64" in error["text"] and "at-spi2-core" in error["text"]
            tool("stop_desktop", {})
        finally:
            server.stdin.close()
            assert server.wait(timeout=15) == 0


screenshot(os.environ.copy())
screenshot(dict(os.environ, SELLAITE_ATSPI_LIBRARY="/missing/libatspi.so.0"))

missing_novnc = subprocess.run(
    ["sellaite", "dashboard", "start-dashboard", "--novnc-dir", "/missing/novnc"],
    capture_output=True, text=True, timeout=5,
)
assert missing_novnc.returncode == 255 and "Ubuntu package: novnc" in missing_novnc.stderr

with subprocess.Popen(
    ["sellaite", "dashboard", "start-dashboard", "--address", "127.0.0.1:0"],
    stdout=subprocess.PIPE,
    text=True,
) as dashboard:
    try:
        url = json.loads(dashboard.stdout.readline())["Url"]
        browser = urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()),
        )
        with browser.open(url, timeout=5) as page:
            assert page.status == 200
        asset = urllib.parse.urljoin(url, "/novnc/core/rfb.js")
        with browser.open(asset, timeout=5) as response:
            assert response.read()
        print("Dashboard served the packaged noVNC client", flush=True)
    finally:
        dashboard.terminate()
        dashboard.wait(timeout=5)
