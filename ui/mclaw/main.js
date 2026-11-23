import QRCode from './vendor/qrcode.js';
import { request } from './api.js';

const $ = id => document.getElementById(id);
let flow = '';
let epoch = 0;
let busy = false;
let pollTimer;
let clockTimer;
let deadline = 0;

function showError(error) {
  $('error').hidden = !error;
  $('error').textContent = error instanceof Error ? error.message : String(error || '');
}
function controls() {
  $('generate').disabled = busy;
  $('generate').textContent = flow ? '刷新二维码' : '生成二维码';
  $('cancel').disabled = busy || !flow;
  $('qr-tab').disabled = busy;
  $('manual-tab').disabled = busy;
  $('proxy-mode').disabled = busy || Boolean(flow);
  for (const input of $('manual-form').elements) input.disabled = busy;
}
function clearFlow() {
  flow = '';
  epoch += 1;
  clearTimeout(pollTimer);
  clearInterval(clockTimer);
  pollTimer = undefined;
  clockTimer = undefined;
  $('qr-image').hidden = true;
  $('qr-image').removeAttribute('src');
  $('qr-empty').hidden = false;
  $('qr-empty').textContent = '点击生成二维码';
  $('qr-box').dataset.state = '';
  $('countdown').textContent = '';
  controls();
}
function confirmed(value) {
  const session = value?.session;
  return value?.done === true && session && (!session.provider || session.provider === 'mclaw')
    && [session.accountUid, session.accountId].some(id => typeof id === 'string' && id.length > 0);
}
function success() {
  clearFlow();
  $('qr-empty').textContent = '登录成功';
  $('qr-status').textContent = '账户已保存。请回到账户列表刷新模型目录。';
  showError('');
}
async function cancelActive() {
  if (!flow) return;
  const id = flow;
  const result = await request('/api/session/login/cancel', { method: 'POST', body: { state: id } });
  if (result?.canceled === true) {
    if (flow === id) clearFlow();
    return 'cancelled';
  }
  try {
    const state = await request('/api/session/login/wait?state=' + encodeURIComponent(id));
    if (confirmed(state)) { success(); return 'completed'; }
    if (state?.done === true) {
      clearFlow();
      throw new Error(state.error || '登录流程已结束，请重新生成二维码');
    }
  } catch (error) {
    if (error.status === 404) {
      clearFlow();
      throw new Error('登录流程已结束，请先检查账户列表，再决定是否重新扫码');
    }
    throw error;
  }
  throw new Error('网关尚未确认取消，仍在等待登录结果，请稍后重试');
}
function selectedProxy() {
  return $('proxy-mode').value === 'direct' ? { proxy: { source: 'direct' } } : {};
}
function qrContent(instructions) {
  const text = typeof instructions === 'string' ? instructions.split('二维码内容：')[1]?.trim() : '';
  if (!text) throw new Error('插件未返回二维码，请确认插件已更新');
  const url = new URL(text);
  const fragment = url.hash.split('?');
  const params = new URLSearchParams(fragment[1]);
  if (url.origin !== 'https://yun.139.com' || url.pathname !== '/w/' || url.search
      || fragment[0] !== '#/qrcLogin' || fragment.length !== 2 || Array.from(params).length !== 3
      || !/^[A-Za-z0-9]{16}$/.test(params.get('sID') || '')
      || !/^[0-9a-f]{32}$/.test(params.get('dID') || '') || params.get('cType') !== '9') {
    throw new Error('二维码内容无效，请刷新后重试');
  }
  return text;
}
function schedulePoll(id, generation) {
  clearTimeout(pollTimer);
  pollTimer = setTimeout(() => void poll(id, generation), 1800);
}
async function poll(id, generation) {
  if (flow !== id || epoch !== generation) return;
  try {
    const state = await request('/api/session/login/wait?state=' + encodeURIComponent(id));
    if (flow !== id || epoch !== generation) return;
    if (state?.error) { clearFlow(); throw new Error(state.error); }
    if (confirmed(state)) { success(); return; }
    if (state?.done === true) { clearFlow(); throw new Error('网关未确认账户已保存，请检查账户列表'); }
    const hint = typeof state?.hint === 'string' ? state.hint : '等待扫码，请在手机上完成确认';
    $('qr-status').textContent = hint;
    $('qr-box').dataset.state = hint.includes('已扫码') ? 'scanned' : '';
    showError('');
  } catch (error) {
    if (flow !== id || epoch !== generation) {
      if (!flow) showError(error);
      return;
    }
    if (error.status === 404) {
      clearFlow();
      showError('登录流程已结束，请检查账户列表；如未添加，请重新扫码。');
      return;
    }
    showError(error);
    $('qr-status').textContent = '暂时无法获取登录进度，正在重试。';
  }
  if (flow === id && epoch === generation) schedulePoll(id, generation);
}
async function expire() {
  if (!flow || busy) return;
  busy = true;
  controls();
  try {
    const result = await cancelActive();
    if (result !== 'completed') $('qr-status').textContent = '二维码已过期，请重新生成。';
  } catch (error) { showError(error); }
  finally { busy = false; controls(); }
}
async function generate() {
  if (busy) return;
  busy = true;
  controls();
  showError('');
  try {
    if (await cancelActive() === 'completed') return;
    $('qr-status').textContent = '正在生成二维码…';
    const started = await request('/api/session/login/start', { method: 'POST', body: { provider: 'mclaw', ...selectedProxy() } });
    const id = started?.flow || started?.state;
    if (typeof id !== 'string' || !/^[A-Za-z0-9]{32}$/.test(id)) throw new Error('网关没有返回有效的登录流程');
    flow = id;
    const generation = ++epoch;
    const content = qrContent(started.instructions);
    const image = await QRCode.toDataURL(content, { width: 320, margin: 4, errorCorrectionLevel: 'M' });
    if (flow !== id || epoch !== generation) return;
    $('qr-image').src = image;
    $('qr-image').hidden = false;
    $('qr-empty').hidden = true;
    $('qr-status').textContent = '请使用移动云盘 App 扫码，并在手机上确认。';
    deadline = Date.now() + 300000;
    const tick = () => {
      if (flow !== id || epoch !== generation) return;
      const seconds = Math.max(0, Math.ceil((deadline - Date.now()) / 1000));
      $('countdown').textContent = '二维码 ' + seconds + ' 秒后失效';
      if (!seconds) void expire();
    };
    tick();
    clockTimer = setInterval(tick, 1000);
    schedulePoll(id, generation);
  } catch (error) {
    if (flow) {
      try { await cancelActive(); }
      catch { schedulePoll(flow, epoch); }
    }
    showError(error);
    $('qr-status').textContent = flow ? '登录流程仍在等待，可再次尝试取消。' : '未能生成二维码，请重试。';
  } finally { busy = false; controls(); }
}
async function chooseTab(manual) {
  if (busy) return;
  busy = true;
  controls();
  try {
    await cancelActive();
    $('qr-panel').hidden = manual;
    $('manual-panel').hidden = !manual;
    $('qr-tab').setAttribute('aria-selected', String(!manual));
    $('manual-tab').setAttribute('aria-selected', String(manual));
    showError('');
  } catch (error) { showError(error); }
  finally { busy = false; controls(); }
}
$('generate').addEventListener('click', () => void generate());
$('cancel').addEventListener('click', async () => {
  if (busy) return;
  busy = true;
  controls();
  try { await cancelActive(); if ($('qr-empty').textContent !== '登录成功') $('qr-status').textContent = '已取消登录。'; showError(''); }
  catch (error) { showError(error); }
  finally { busy = false; controls(); }
});
$('qr-tab').addEventListener('click', () => void chooseTab(false));
$('manual-tab').addEventListener('click', () => void chooseTab(true));
$('manual-method').addEventListener('change', () => {
  const basic = $('manual-method').value === '1';
  $('phone-label').hidden = basic;
  $('phone').required = !basic;
  $('token-label').textContent = basic ? 'Basic 凭证或 Cookie' : '登录 Token';
});
$('manual-form').addEventListener('submit', async event => {
  event.preventDefault();
  if (busy) return;
  busy = true;
  controls();
  showError('');
  $('manual-status').textContent = '正在添加账户…';
  try {
    const methodIndex = Number($('manual-method').value);
    const inputs = { token: $('token').value, channelId: $('channel').value };
    if (methodIndex === 0) inputs.phone = $('phone').value;
    const result = await request('/api/accounts', { method: 'POST', body: {
      provider: 'mclaw', methodIndex, inputs, name: $('name').value, ...selectedProxy()
    } });
    if (!result?.account?.id || result.account.provider !== 'mclaw') throw new Error('网关未确认账户已添加，请检查账户列表');
    $('token').value = '';
    $('manual-status').textContent = '账户已添加。请回到账户列表刷新模型目录。' + (result.warning ? ' ' + result.warning : '');
  } catch (error) { showError(error); $('manual-status').textContent = '未能确认添加成功，请检查账户列表后再重试。'; }
  finally { busy = false; controls(); }
});
window.addEventListener('pagehide', () => {
  if (flow) void request('/api/session/login/cancel', { method: 'POST', body: { state: flow }, keepalive: true }).catch(() => {});
});
controls();
