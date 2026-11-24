import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('..', import.meta.url));
const pack = JSON.parse(readFileSync(resolve(root, 'pack.json'), 'utf8'));
const permissions = JSON.parse(readFileSync(resolve(root, 'wasm.json'), 'utf8'));
const target = 'wasm32-unknown-unknown';
const provider = pack.providers[0].id;
const module = new WebAssembly.Module(readFileSync(resolve(
  process.env.CARGO_TARGET_DIR || resolve(root, 'wasm/target'), target, 'release', pack.entry[target],
)));
assert.deepEqual(WebAssembly.Module.imports(module).map(({ module, name }) => `${module}.${name}`).sort(),
  ['agent2api.call', 'agent2api.read']);
assert.deepEqual(Object.keys(pack.entry), [target]);
assert.equal(pack.providers.length, 1);
assert.equal(pack.ui, undefined);

const encoder = new TextEncoder();
const decoder = new TextDecoder('utf-8', { fatal: true });
const account = { id: 'account-fixture', secret: {}, public: { enabled: true } };
let ledger = null;
let requestCount = 0;
const state = new Map();
const sessions = new Map();
const NOW = Date.UTC(2026, 0, 2, 12);
let stream;
let fixture;

function host(method, params) {
  switch (method) {
    case 'account.get': return account;
    case 'account.update':
      assert.deepEqual(params.expect, account.secret);
      account.secret = params.secret;
      return { updated: true };
    case 'clock.now': return NOW;
    case 'random.bytes': return Buffer.alloc(params.length, 1).toString('base64');
    case 'lock.acquire': return 'lock-fixture';
    case 'lock.release': return null;
    case 'state.get': return state.get(`${params.scope}/${params.key}`) || { revision: 0, schema: 0, value: null, compatible: true };
    case 'state.compare_exchange': {
      const key = `${params.scope}/${params.key}`;
      assert.equal(state.get(key)?.revision || 0, params.revision);
      state.set(key, { revision: params.revision + 1, schema: params.schema, value: params.value, compatible: true });
      return { updated: true };
    }
    case 'session.get': return sessions.get(params.id) || null;
    case 'session.put': sessions.set(params.id, params.value); return null;
    case 'ledger.get': return ledger;
    case 'ledger.put': ledger = params.value; return null;
    case 'catalog.get': return { models: [], meta: { remote: false, refreshed_at: 0 } };
    case 'http.open': {
      assert(permissions.origins.includes(new URL(params.url).origin), 'request must match WASM permission manifest');
      assert.equal(params.proxy, undefined);
      assert.equal(params.headers?.Cookie, undefined);
      const body = params.body == null ? null : JSON.parse(Buffer.from(params.body, 'base64').toString());
      const { status = 200, mime = 'application/json', data, pieces } = fixture(params, body);
      const id = String(++requestCount);
      stream.responses.set(id, pieces || [Buffer.from(JSON.stringify(data))]);
      return { id, status, headers: { 'content-type': mime }, headers_raw: [['content-type', mime]] };
    }
    case 'http.read': {
      const bytes = stream.responses.get(params.id).shift();
      return bytes ? { data: bytes.toString('base64'), done: false } : { done: true };
    }
    case 'http.close': stream.responses.delete(params.id); return null;
    case 'stream.head':
      assert.equal(stream.head, null);
      stream.head = params;
      return null;
    case 'stream.chunk':
      assert(stream.head);
      stream.parts.push(Buffer.from(params.data, 'base64'));
      return null;
    default: throw new Error(`unexpected host method ${method}`);
  }
}

function call(method, params = {}) {
  stream = { head: null, parts: [], responses: new Map() };
  let pending;
  const instance = new WebAssembly.Instance(module, { agent2api: {
    call(pointer, length) {
      const { method, params } = JSON.parse(decoder.decode(memory(pointer, length)));
      pending = encoder.encode(JSON.stringify({ result: host(method, params) }));
      return pending.byteLength;
    },
    read(pointer, length) { memory(pointer, length).set(pending); return length; },
  } });
  const { a2a_alloc, a2a_call, a2a_abi_version } = instance.exports;
  function memory(pointer, length) { return new Uint8Array(instance.exports.memory.buffer, pointer, length); }
  assert.equal(a2a_abi_version(), permissions.abi);
  const input = encoder.encode(JSON.stringify({ method, params: { provider, ...params } }));
  const ptr = a2a_alloc(input.length);
  memory(ptr, input.length).set(input);
  const output = a2a_call(ptr, input.length);
  return JSON.parse(decoder.decode(memory(Number(output & 0xffff_ffffn), Number(output >> 32n))));
}

function result(method, params) {
  const reply = call(method, params);
  assert(!reply.error, reply.error?.message);
  return reply.result;
}

function manual(payload) {
  const added = result('account.normalize', { source: { kind: 'manual', payload } });
  account.secret = added.secret;
  assert.equal(result('account.identity', { secret: account.secret }), added.identity);
  assert.deepEqual(result('account.view', { secret: account.secret }), added.public);
  assert(!JSON.stringify(added.public).includes('fixture-secret'));
}

function chat(streaming = false) {
  result('conversation', { account: account.id, stream: streaming, wire_model: 'upstream-model', body: {
    model: 'client-model', messages: [{ role: 'user', content: 'hello' }],
  } });
  const output = Buffer.concat(stream.parts).toString();
  assert.equal(stream.head.status, 200);
  assert.equal(stream.head.headers[0][1], streaming ? 'text/event-stream' : 'application/json');
  if (streaming) assert(output.endsWith('data: [DONE]\n\n'));
  else assert.equal(JSON.parse(output).choices[0].message.content, '你好');
}

const [meta] = result('init');
assert.equal(meta.id, provider);
assert.equal(meta.capabilities.chat, true);
assert(result('admin.action', { action: 'auth.methods' }).methods.some(m => m.type === 'api'));
assert.deepEqual(result('models.list').models, []);
let keys = 0;
let modelUrl = 'https://ai.yun.139.com/api/llm/102001/v1/chat/completions';
fixture = ({ url, headers }, body) => {
  const path = new URL(url).pathname;
  switch (path) {
    case '/api/llm/apikey/get':
      assert(headers.authorization.startsWith('Basic '));
      assert.equal(body.channelId, '102001');
      keys++;
      return { data: { success: true, data: { apikey: 'fixture-secret-llm', modelUrl,
        modelList: [{ modelCode: 'upstream-model', modelName: 'Test' }],
      } } };
    case '/api/adapter/assistant/agent/list': return { data: { success: true, data: { agentList: [] } } };
    case '/isbo2/openApi/queryAvailableBenefitV2': return { data: { result: { resultCode: '0' },
      userAvailableBenefitList: [{ benefitNo: 'RHR135', benefitValue: '10', benefitUsed: '2' }],
    } };
    case '/api/llm/102001/v1/chat/completions':
      assert.equal(headers.authorization, 'Bearer fixture-secret-llm');
      assert.equal(body.model, 'upstream-model');
      if (body.stream) return { mime: 'text/event-stream', pieces: [Buffer.from('data: '), Buffer.from('[DONE]\n\n')] };
      return { data: { choices: [{ message: { content: '你好' } }] } };
    default: throw new Error(`unexpected path ${path}`);
  }
};
manual({ methodIndex: 0, inputs: { phone: '13800138000', token: 'fixture-secret|session' } });
assert(result('models.refresh', { account: account.id }).models.some(m => m.id === 'upstream-model'));
assert.equal(result('usage.query').windows[0].remaining, 8);
chat();
chat(true);
assert.equal(keys, 1);
modelUrl = 'https://ai.yun.139.com.evil.invalid/chat';
assert.equal(call('token.refresh').error?.status, 502);
console.log('MClaw WASM fixture checks passed');
