const panelHeaders = new Headers();
let credentialsReady;
let ready;
const credentials = new Promise(resolve => { ready = resolve; });
window.addEventListener('message', event => {
  if (event.source !== window.parent || !event.data || typeof event.data !== 'object') return;
  const data = event.data;
  if (data.type === 'agent2api:panel-theme') {
    document.documentElement.dataset.theme = ['light', 'dark'].includes(data.theme) ? data.theme : 'system';
  }
  if (data.type === 'agent2api:panel-credentials' && data.headers && typeof data.headers === 'object') {
    for (const [name, value] of Object.entries(data.headers)) {
      if (typeof value === 'string' && value) {
        try { panelHeaders.set(name, value); } catch { /* Reject malformed header names. */ }
      }
    }
    credentialsReady = true;
    ready();
  }
});
if (window.parent !== window) {
  window.parent.postMessage({ type: 'agent2api:request-credentials' }, '*');
  window.parent.postMessage({ type: 'agent2api:request-theme' }, '*');
}
setTimeout(ready, 3000);

export async function request(path, options = {}) {
  if (!credentialsReady) await credentials;
  const headers = new Headers(panelHeaders);
  headers.set('Accept', 'application/json');
  const init = { method: options.method || 'GET', headers, credentials: 'include', keepalive: Boolean(options.keepalive) };
  if (options.body !== undefined) {
    headers.set('Content-Type', 'application/json');
    init.body = JSON.stringify(options.body);
  }
  const response = await fetch(path, init);
  let envelope;
  try { envelope = await response.json(); }
  catch { throw new Error('网关返回了无效响应，请检查连接后重试'); }
  if (!response.ok || envelope?.success === false || envelope?.error) {
    const detail = envelope?.error;
    const error = new Error(typeof detail === 'string' ? detail : detail?.message || '网关请求失败（HTTP ' + response.status + '）');
    error.status = response.status;
    throw error;
  }
  return Object.hasOwn(envelope, 'success') ? envelope.data : envelope;
}
