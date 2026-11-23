#!/usr/bin/env python3
"""Exercise the real plugin process against a local mock MClaw upstream."""
import argparse
import base64
import collections
import http.server
import json
import os
import platform
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
PHONE = "13800138000"
TOKEN = "offline-fixture|1|RCS|1795259045072|example-"
COUNTS = collections.Counter()
REQUESTS = []
FAIL = {"key": False, "agent": False, "quota": False, "bad_url": False}


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        path = urllib.parse.urlsplit(self.path).path
        COUNTS[path] += 1
        REQUESTS.append((self.path, dict(self.headers), body))
        assert self.headers.get("x-yun-api-version") == "v1" or path.endswith("/chat/completions")
        if path.endswith("/chat/completions"):
            assert self.headers.get("Authorization", "").startswith("Bearer mock-key-")
            if body.get("stream"):
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                chunk = {"choices": [{"delta": {"content": "离线测试成功"}, "finish_reason": None}]}
                self.wfile.write(("data: " + json.dumps(chunk, ensure_ascii=False) + "\n\ndata: [DONE]\n\n").encode())
                return
            value = {"choices": [{"message": {"role": "assistant", "content": "离线测试成功"}}], "usage": {"prompt_tokens": 2, "completion_tokens": 3}}
        elif path == "/api/llm/apikey/get":
            assert body == {"channelId": "102001"}
            if FAIL["key"]:
                value = {"success": False, "code": "01000003", "message": "expired"}
            else:
                value = {"success": True, "data": {"apikey": "mock-key-" + str(COUNTS[path]), "modelUrl": "http://unsafe.invalid/chat/completions" if FAIL["bad_url"] else BASE + "/api/llm/102001/v1/chat/completions", "modelList": [{"modelCode": "test-model", "modelName": "测试模型", "tokenRatio": "0.5"}]}}
        elif path == "/api/adapter/assistant/agent/list":
            assert self.headers.get("x-yun-app-channel") == "101"
            assert self.headers.get("origin") == "mcloudlocal://yun.139.com"
            value = {"success": not FAIL["agent"], "data": {"agentList": [{"agentType": "other", "modelList": [{"modelCode": "excluded"}]}, {"agentType": "mclaw", "modelList": [{"modelCode": "test-model"}, {"modelCode": "agent-model", "description": "Agent 模型"}]}]}}
        elif path == "/isbo2/openApi/queryAvailableBenefitV2":
            assert body == {"account": {"userDomainId": ""}}
            value = {"result": {"resultCode": "0"}, "userAvailableBenefitList": [] if FAIL["quota"] else [{"benefitNo": "RHR135", "benefitValue": "1000", "benefitUsed": "125"}]}
        else:
            raise AssertionError(path)
        encoded = json.dumps(value, ensure_ascii=False).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)


class RPC:
    def __init__(self, exe, data_dir):
        env = {**os.environ, "MCLAW_TEST_AI_BASE": BASE, "MCLAW_TEST_BENEFIT_BASE": BASE}
        self.process = subprocess.Popen([str(exe)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, bufsize=1, env=env)
        self.frames = queue.Queue()
        def read():
            for line in self.process.stdout:
                self.frames.put(json.loads(line))
        threading.Thread(target=read, daemon=True).start()
        self.next_id = 0
        self.store = {}
        self.data_dir = data_dir

    def write(self, frame):
        self.process.stdin.write(json.dumps(frame) + "\n")
        self.process.stdin.flush()

    def call(self, method, params=None, error=False):
        self.next_id += 1
        request_id = self.next_id
        self.write({"id": request_id, "method": method, "params": params or {"provider": "mclaw", "account": "one"}})
        while True:
            frame = self.frames.get(timeout=20)
            if "method" in frame:
                payload = frame.get("params", {})
                if frame["method"] == "store.get":
                    value = self.store[payload["account"]]
                elif frame["method"] == "store.list":
                    value = list(self.store.values())
                elif frame["method"] == "notify":
                    value = None
                else:
                    raise AssertionError(frame)
                self.write({"id": frame["id"], "result": value})
            elif frame.get("id") == request_id and ("result" in frame or "error" in frame):
                if error:
                    assert "error" in frame, frame
                    return frame["error"]
                assert "error" not in frame, frame
                return frame["result"]

    def init(self, proxy="direct"):
        arch = "aarch64" if platform.machine() in ("aarch64", "arm64") else "x86_64"
        target = arch + ("-apple-darwin" if sys.platform == "darwin" else "-unknown-linux-gnu")
        return self.call("init", {"env": {"data_dir": self.data_dir, "host_version": "integration-test", "target": target, "proxy": proxy, "debug_traffic": False}})

    def close(self):
        if self.process.poll() is None:
            self.process.terminate()
            self.process.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", type=Path, default=ROOT / "target/release/agent2api-mclaw")
    args = parser.parse_args()
    global BASE
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    BASE = "http://127.0.0.1:" + str(server.server_port)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    passed = []
    def check(name, condition):
        assert condition, name
        passed.append(name)
        print("PASS", name)
    with tempfile.TemporaryDirectory(prefix="mclaw-verify-") as directory:
        rpc = RPC(args.exe.resolve(), directory)
        try:
            info = rpc.init()[0]
            check("SDK handshake and truthful capabilities", info["id"] == "mclaw" and info["capabilities"]["chat"] and info["capabilities"]["usage"] and info["capabilities"]["web_login"] and not info["capabilities"]["refresh"] and not info["capabilities"]["checkin"])
            metadata = rpc.call("admin.action", {"provider": "mclaw", "action": "auth.methods", "payload": {}})
            methods = metadata["methods"]
            check("dynamic account forms available without legacy fallback", [m["methodIndex"] for m in methods] == [0, 1] and all(m["type"] == "api" for m in methods))
            check("form metadata has no secret defaults", all("default" not in p for m in methods for p in m["prompts"] if p.get("secret")))
            check("primary form requires phone and masked token", {p["key"] for p in methods[0]["prompts"] if not p.get("optional")} == {"phone", "token"} and next(p for p in methods[0]["prompts"] if p["key"] == "token")["secret"])
            normalized = rpc.call("account.normalize", {"provider": "mclaw", "source": {"kind": "manual", "payload": {"phone": PHONE, "token": TOKEN}}, "name": "测试账号"})
            check("credential import stays local", not COUNTS and normalized["secret"]["token"] == TOKEN)
            fields = {p["key"]: p.get("default", "") for p in methods[0]["prompts"]}
            fields.update({"phone": PHONE, "token": TOKEN})
            form_account = rpc.call("account.normalize", {"provider": "mclaw", "name": "表单账号", "source": {"kind": "manual", "payload": {"methodIndex": 0, "inputs": fields}}})
            check("gateway dynamic form envelope imports the same account", form_account["identity"] == normalized["identity"] and form_account["secret"]["channelId"] == "102001" and form_account["public"]["name"] == "表单账号")
            check("public view excludes full phone and token", PHONE not in json.dumps(normalized["public"]) and TOKEN not in json.dumps(normalized["public"]))
            basic = "Basic " + base64.b64encode(f"pc:{PHONE}:{TOKEN}".encode()).decode()
            imported = rpc.call("account.normalize", {"provider": "mclaw", "source": {"kind": "manual", "payload": {"methodIndex": 1, "inputs": {"token": basic, "channelId": "102001"}}}})
            check("Basic form automatically extracts phone", imported["identity"] == normalized["identity"] and imported["secret"]["phone"] == PHONE)
            for label, payload in [
                ("missing phone", {"methodIndex": 0, "inputs": {"token": TOKEN}}),
                ("unknown method", {"methodIndex": 9, "inputs": {"token": TOKEN}}),
                ("nonstring token", {"methodIndex": 0, "inputs": {"phone": PHONE, "token": 123}}),
                ("unknown field", {"methodIndex": 1, "inputs": {"token": basic, "unexpected": "x"}}),
            ]:
                error = rpc.call("account.normalize", {"provider": "mclaw", "source": {"kind": "manual", "payload": payload}}, error=True)
                check("form validation " + label, bool(error) and TOKEN not in str(error) and basic not in str(error))
            for source in [{"kind": "file_text", "content": basic}, {"kind": "file_base64", "content": base64.b64encode(json.dumps({"phone": PHONE, "token": TOKEN}).encode()).decode()}]:
                result = rpc.call("account.normalize", {"provider": "mclaw", "source": source})
                check("file import " + source["kind"], result["identity"] == normalized["identity"])
            check("malformed credentials rejected", bool(rpc.call("account.normalize", {"provider": "mclaw", "source": {"kind": "manual", "payload": {"phone": PHONE, "token": "bad"}}}, error=True)))
            rpc.store["one"] = {"id": "one", "provider": "mclaw", "secret": normalized["secret"], "public": normalized["public"], "proxy": {"state": "unset"}}
            first_key = rpc.call("token.ensure")
            check("LLM key acquired through store callback", first_key == "mock-key-1")
            body = {"model": "alias", "messages": [{"role": "user", "content": "你好"}], "tools": [{"type": "function", "function": {"name": "echo", "parameters": {"type": "object"}}}], "temperature": 0.4, "max_tokens": 20}
            params = {"provider": "mclaw", "account": "one", "proxy": None, "headers": {}, "body": body, "wire_model": "test-model", "reasoning": "", "stream": True}
            plan = rpc.call("chat", params)
            check("chat plan retains tools and generation parameters", plan["body"]["tools"] == body["tools"] and plan["body"]["temperature"] == 0.4 and plan["body"]["max_tokens"] == 20 and plan["body"]["model"] == "test-model" and plan["body"]["stream"])
            check("five minute LLM key cache", COUNTS["/api/llm/apikey/get"] == 1)
            request = urllib.request.Request(plan["url"], data=json.dumps(plan["body"]).encode(), headers=dict(plan["headers"]))
            opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
            stream = opener.open(request, timeout=5).read().decode()
            check("stream plan produces valid UTF-8 SSE", "离线测试成功" in stream and "data: [DONE]" in stream)
            plan = rpc.call("chat", {**params, "stream": False})
            request = urllib.request.Request(plan["url"], data=json.dumps(plan["body"]).encode(), headers=dict(plan["headers"]))
            response = json.load(opener.open(request, timeout=5))
            check("nonstream plan preserves usage", response["usage"]["completion_tokens"] == 3 and not plan["body"]["stream"])
            catalog = rpc.call("models.list")["models"]
            ids = [row["id"] for row in catalog]
            check("remote catalogs merge and deduplicate only MClaw agents", "agent-model" in ids and "excluded" not in ids and ids.count("test-model") == 1 and len(ids) == 4)
            check("token ratio preserved", next(row["tokenRatio"] for row in catalog if row["id"] == "test-model") == 0.5)
            FAIL["agent"] = True
            check("agent endpoint failure preserves primary catalog", "test-model" in [row["id"] for row in rpc.call("models.list")["models"]])
            FAIL["agent"] = False
            check("force refresh reacquires key", rpc.call("models.refresh", {"provider": "mclaw", "account": "one", "force": True})["refreshed"] and COUNTS["/api/llm/apikey/get"] == 2)
            quota = rpc.call("usage.query")["windows"][0]
            check("aitokens quota parses numeric strings", quota["total"] == 1000 and quota["used"] == 125 and quota["remaining"] == 875 and quota["percent"] == 12.5)
            FAIL["quota"] = True
            check("missing benefit reports error", bool(rpc.call("usage.query", error=True)))
            FAIL["quota"] = False
            refreshed = rpc.call("token.refresh")
            check("explicit refresh bypasses key cache", refreshed != first_key and COUNTS["/api/llm/apikey/get"] == 3)
            rpc.store["one"]["secret"]["token"] = "rotated|session"
            check("credential rotation invalidates key cache", rpc.call("token.ensure") != refreshed and COUNTS["/api/llm/apikey/get"] == 4)
            rpc.store["two"] = {**rpc.store["one"], "id": "two"}
            rpc.call("token.ensure", {"provider": "mclaw", "account": "two"})
            check("account key caches are isolated", COUNTS["/api/llm/apikey/get"] == 5)
            rpc.store["one"]["proxy"] = {"state": "failed", "message": "no proxy"}
            check("failed account proxy never silently bypassed", rpc.call("token.ensure", error=True)["code"] == "local_precondition")
            check("explicit direct proxy override works", rpc.call("chat", {**params, "proxy_override": True})["upstream"] == "chat")
            rpc.store["one"]["proxy"] = {"state": "unset"}
            rpc.init(BASE)
            rpc.call("token.ensure")
            check("gateway proxy reaches upstream", any(path.startswith(BASE + "/api/llm/apikey/get") for path, _, _ in REQUESTS))
            rpc.init("direct")
            FAIL["key"] = True
            error = rpc.call("token.refresh", error=True)
            check("upstream expired login retains HTTP 401 metadata", "401" in error.get("code", "") and TOKEN not in str(error))
            FAIL["key"] = False
            FAIL["bad_url"] = True
            check("insecure nonlocal model URL rejected", bool(rpc.call("token.refresh", error=True)))
            FAIL["bad_url"] = False
            for status, kind in [(401, "token_expired"), (402, "quota_limited"), (429, "quota_limited"), (500, "fatal")]:
                check("upstream error classification " + str(status), rpc.call("classify", {"provider": "mclaw", "status": status, "body": {"message": "test"}})["kind"] == kind)
            check("public account API redacts credentials", TOKEN not in json.dumps(rpc.call("account.view", {"provider": "mclaw", "secret": normalized["secret"]})))
            check("graceful SDK shutdown", rpc.call("shutdown", {}) is None)
            rpc.process.wait(timeout=5)
            check("clean exit and no credential logs", rpc.process.returncode == 0 and TOKEN not in rpc.process.stderr.read())
        finally:
            rpc.close()
            server.shutdown()
            server.server_close()
    print(f"{len(passed)} checks passed; all requests used a local fixture, no live account required.")


if __name__ == "__main__":
    main()
