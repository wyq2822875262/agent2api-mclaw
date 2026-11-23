#!/usr/bin/env python3
"""Verify QR login RPCs against independent OpenSSL-backed local upstream."""
import argparse
import base64
import hashlib
import http.server
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import threading
import time
import urllib.parse

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("mclaw_verify", ROOT / "scripts/verify-mclaw.py")
verify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify)
WIRE_KEY = b"UqEZkrjCKfa02pP6jntzFmkzOz86zHUC"
MODE = "pending"
ERRORS = []
SEEN = []
ENTERED = threading.Event()
RELEASE = threading.Event()


def crypt(data, decrypt=False, key=WIRE_KEY, iv=bytes(range(16)), ecb=False):
    command = ["openssl", "enc", "-aes-128-ecb" if ecb else "-aes-256-cbc", "-K", key.hex()]
    if not ecb:
        command += ["-iv", iv.hex()]
    if decrypt:
        command += ["-d"]
    return subprocess.run(command, input=data, capture_output=True, check=True).stdout


def wire(value):
    iv = bytes(range(16))
    data = json.dumps(value, separators=(",", ":")).encode()
    return base64.b64encode(iv + crypt(data, iv=iv))


def signature(body, timestamp, nonce):
    encoded = urllib.parse.quote(json.dumps(body, separators=(",", ":"), ensure_ascii=False), safe="-_.~")
    for char in "!'()*":
        encoded = encoded.replace("%%%02X" % ord(char), "%%%02x" % ord(char))
    md5 = lambda data: hashlib.md5(data).hexdigest()
    first = md5(base64.b64encode("".join(sorted(encoded)).encode()))
    second = md5((timestamp + ":" + nonce).encode())
    return md5((first + second).encode()).upper()


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        try:
            self.process()
        except Exception as error:
            ERRORS.append(repr(error))
            self.send_error(500)

    def process(self):
        raw = self.rfile.read(int(self.headers["Content-Length"]))
        rsa_request = self.path.endswith("/getRsaPublicKey")
        if rsa_request:
            assert not self.headers.get("hcy-cool-flag")
            body = json.loads(raw)
            assert body == {"clientCode": "10701", "type": 1}
            result = {"success": True, "data": {"publicKey": PUBLIC_KEY}}
        else:
            assert self.path == "/user/thirdlogin"
            assert self.headers.get("hcy-cool-flag") == "1"
            packed = base64.b64decode(raw, validate=True)
            body = json.loads(crypt(packed[16:], decrypt=True, iv=packed[:16]))
            assert body["pintype"] == 21 and body["cpid"] == 292 and body["clienttype"] == 670
            assert body["version"] == "mCloud_4.3.0_536" and body["loginMode"] == "0"
            assert body["msisdn"] == body["random"] == "" and body["extInfo"] == {}
            assert re.fullmatch("[A-Za-z0-9]{16}", body["dycpwd"])
            assert body["secinfo"] == hashlib.sha1(("fetion.com.cn:" + body["dycpwd"]).encode()).hexdigest().upper()
            statuses = {"pending": "200059541", "scanned": "200059548", "expired": "200059542", "cancelled": "200059549", "unknown": "unknown"}
            if MODE in ("done", "blocked_done"):
                if MODE == "blocked_done":
                    ENTERED.set()
                    assert RELEASE.wait(5)
                secret = {"account": "13800138000", "token": "fixture|qr-login"}
                encrypted = crypt(json.dumps(secret).encode(), key=b"qPqDw263XgFgL3u8", ecb=True)
                result = {"success": True, "data": encrypted.hex()}
            elif MODE == "network":
                self.send_error(503)
                return
            elif MODE == "incomplete":
                result = {"success": True, "data": {}}
            else:
                result = {"success": False, "code": statuses[MODE], "message": "private-upstream-text"}
        device = self.headers["x-deviceinfo"]
        visitor = device.split("|")[6]
        assert re.fullmatch("[0-9a-f]{32}", visitor)
        assert self.headers["x-yun-client-info"] == device + "dW5kZWZpbmVk||"
        assert self.headers["mcloud-client"] == "10701" and self.headers["mcloud-channel"] == "1000101"
        stamp, nonce, digest = self.headers["mcloud-sign"].split(",")
        assert re.fullmatch("[A-Za-z0-9]{16}", nonce)
        assert digest == signature(body, stamp, nonce)
        SEEN.append({"rsa": rsa_request, "body": body, "visitor": visitor})
        data = json.dumps(result).encode() if rsa_request else wire(result)
        self.send_response(200)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", type=Path, required=True)
    args = parser.parse_args()
    global PUBLIC_KEY, MODE
    with tempfile.TemporaryDirectory(prefix="mclaw-qr-test-") as directory:
        private_key = Path(directory) / "fixture.key"
        subprocess.run(["openssl", "genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out", str(private_key)], check=True, capture_output=True)
        der = subprocess.run(["openssl", "pkey", "-in", str(private_key), "-pubout", "-outform", "DER"], check=True, capture_output=True).stdout
        PUBLIC_KEY = base64.b64encode(der).decode()
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        base = "http://127.0.0.1:" + str(server.server_port)
        verify.BASE = base
        os.environ["MCLAW_TEST_QR_BASE"] = base
        rpc = verify.RPC(args.exe.resolve(), directory)
        passed = []
        def check(label, condition):
            assert condition, label
            passed.append(label)
            print("PASS", label, flush=True)
        def start():
            value = rpc.call("login.start", {"provider": "mclaw", "method": "", "payload": {}})
            check("QR start provides a bounded public session", value["url"] == "" and value["poll_interval_ms"] == 2500 and re.fullmatch("[A-Za-z0-9]{32}", value["flow"]) is not None)
            content = value["instructions"].split("二维码内容：")[1]
            params = urllib.parse.parse_qs(urllib.parse.urlsplit(content).fragment.split("?")[1])
            check("QR device matches request headers", params["dID"][0] == SEEN[-1]["visitor"] and re.fullmatch("[A-Za-z0-9]{16}", params["sID"][0]) is not None)
            return value["flow"]
        def poll(flow):
            return rpc.call("login.poll", {"provider": "mclaw", "flow": flow})
        try:
            info = rpc.init()[0]
            check("web login capability declared", info["capabilities"]["web_login"])
            flow = start()
            check("unscanned remains pending", poll(flow)["state"] == "pending")
            MODE = "scanned"
            time.sleep(2.55)
            check("scanned asks for phone confirmation", "已扫码" in poll(flow)["hint"])
            MODE = "done"
            time.sleep(2.55)
            account = poll(flow)
            check("encrypted confirmation returns normalized account", account["state"] == "done" and account["account"]["secret"]["token"] == "fixture|qr-login")
            public = json.dumps(account["account"]["public"])
            check("confirmation public view excludes credentials", "fixture|qr-login" not in public and "13800138000" not in public)
            check("completed flow cannot be replayed", poll(flow)["state"] == "failed")
            flow = start()
            rpc.call("login.cancel", {"provider": "mclaw", "flow": flow})
            check("cancelled flow cannot return credentials", poll(flow)["state"] == "failed")
            for mode in ["expired", "cancelled", "unknown", "incomplete"]:
                MODE = mode
                flow = start()
                result = poll(flow)
                check(mode + " stops login without leaking upstream text", result["state"] == "failed" and "private-upstream-text" not in str(result))
            MODE = "network"
            flow = start()
            check("first network failure is retried", poll(flow)["state"] == "pending")
            time.sleep(2.55)
            check("second network failure is retried", poll(flow)["state"] == "pending")
            time.sleep(2.55)
            check("third network failure terminates", poll(flow)["state"] == "failed")
            MODE = "blocked_done"
            flow = start()
            rpc.next_id += 1
            poll_id = rpc.next_id
            rpc.write({"id": poll_id, "method": "login.poll", "params": {"provider": "mclaw", "flow": flow}})
            check("in-flight poll reached fixture", ENTERED.wait(5))
            rpc.call("login.cancel", {"provider": "mclaw", "flow": flow})
            RELEASE.set()
            result = rpc.frames.get(timeout=10)
            check("cancellation rejects late confirmation", result["id"] == poll_id and result["result"]["state"] == "failed" and "secret" not in str(result))
            check("all upstream requests passed independent crypto and header checks", not ERRORS and len(SEEN) > 10)
            rpc.call("shutdown", {})
        finally:
            RELEASE.set()
            rpc.close()
            server.shutdown()
            server.server_close()
            os.environ.pop("MCLAW_TEST_QR_BASE", None)
        print(str(len(passed)) + " QR checks passed; no live account used.")


if __name__ == "__main__":
    main()
