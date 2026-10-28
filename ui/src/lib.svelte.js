// Live state of the bot, fed by one WebSocket. Pure data: components only read from `bot`.
const EVENT_LIMIT = 200;
const BALANCE_POINTS = 720;
const TOKEN_KEY = 'sniper-token';

export const bot = $state({
  token: readToken(),
  authError: '',
  online: false, // websocket open
  snap: null,
  events: [], // newest first
  balance: { t: [], v: [] }, // for the chart, SOL
  createsSeen: 0,
  startLamports: null, // wallet balance when the bot started, to show the drawdown live
});

function readToken() {
  try {
    return sessionStorage.getItem(TOKEN_KEY) || '';
  } catch {
    return '';
  }
}

function saveToken(t) {
  try {
    if (t) sessionStorage.setItem(TOKEN_KEY, t);
    else sessionStorage.removeItem(TOKEN_KEY);
  } catch {
    /* storage can be blocked; the page still works for this session */
  }
}

const headers = () => ({ Authorization: `Bearer ${bot.token}`, 'Content-Type': 'application/json' });

async function call(path, method = 'POST', body) {
  const res = await fetch(path, { method, headers: headers(), body: body ? JSON.stringify(body) : undefined });
  if (!res.ok) throw new Error((await res.text()) || `HTTP ${res.status}`);
  return res;
}

export const saveSettings = (s) => call('/api/settings', 'POST', s);
export const setPaused = (paused) => call('/api/pause', 'POST', { paused });
export const sellNow = (mint) => call(`/api/sell/${mint}`);

function pushBalance(ts, lamports) {
  bot.balance.t.push(ts / 1000);
  bot.balance.v.push(lamports / 1e9);
  if (bot.balance.t.length > BALANCE_POINTS) {
    bot.balance.t.shift();
    bot.balance.v.shift();
  }
}

function loadSnapshot(snap) {
  bot.snap = snap;
  bot.startLamports =
    snap.balance_lamports != null && snap.drawdown_sol != null
      ? snap.balance_lamports + Math.round(snap.drawdown_sol * 1e9)
      : null;
  bot.events = [...snap.recent].reverse();
  bot.createsSeen = snap.recent.filter((e) => e.type === 'create_detected').length;
  bot.balance = { t: [], v: [] };
  for (const e of snap.recent) if (e.type === 'balance') pushBalance(e.ts_ms, e.lamports);
}

function applyEvent(e) {
  const s = bot.snap;
  const pos = (mint) => s.positions.find((p) => p.mint === mint);
  switch (e.type) {
    case 'create_detected':
      bot.createsSeen++;
      break;
    case 'buy_sent':
      s.session_spent_sol += e.sol;
      s.positions.push({
        mint: e.mint, source: e.source, status: 'held', opened_ms: e.ts_ms, sol: e.sol,
        sell_at_slot: e.sell_at_slot, buy_signature: e.signature, sell_signature: null, error: null,
      });
      break;
    case 'sell_started':
      if (pos(e.mint)) { pos(e.mint).status = 'selling'; pos(e.mint).error = null; }
      break;
    case 'sell_sent':
      if (pos(e.mint)) { pos(e.mint).status = 'sold'; pos(e.mint).sell_signature = e.signature; }
      break;
    case 'sell_failed':
      if (pos(e.mint)) { pos(e.mint).status = 'failed'; pos(e.mint).error = e.error; }
      break;
    case 'balance':
      s.balance_lamports = e.lamports;
      if (bot.startLamports == null) bot.startLamports = e.lamports;
      s.drawdown_sol = (bot.startLamports - e.lamports) / 1e9;
      pushBalance(e.ts_ms, e.lamports);
      break;
    case 'stream':
      s.stream_connected = e.connected;
      break;
    case 'paused':
      s.settings.paused = e.paused;
      break;
    case 'settings_changed':
      s.settings = e.settings;
      break;
  }
  bot.events.unshift(e);
  if (bot.events.length > EVENT_LIMIT) bot.events.length = EVENT_LIMIT;
}

let socket = null;
let retry = 1000;
let stopped = true;

export function logout() {
  stopped = true;
  socket?.close();
  bot.token = '';
  bot.snap = null;
  bot.online = false;
  saveToken('');
}

export async function login(token) {
  bot.token = token.trim();
  bot.authError = '';
  const res = await fetch('/api/state', { headers: headers() }).catch(() => null);
  if (!res) {
    bot.authError = 'Cannot reach the bot API.';
    return;
  }
  if (res.status === 401) {
    bot.authError = 'Wrong token.';
    bot.token = '';
    saveToken('');
    return;
  }
  saveToken(bot.token);
  loadSnapshot(await res.json());
  connect();
}

export function connect() {
  stopped = false;
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  socket = new WebSocket(`${proto}://${location.host}/ws?token=${encodeURIComponent(bot.token)}`);
  socket.onopen = () => {
    bot.online = true;
    retry = 1000;
  };
  socket.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (msg.type === 'snapshot') loadSnapshot(msg.data);
    else if (msg.type === 'tick' && bot.snap) {
      bot.snap.slot = msg.slot;
      for (const e of msg.events) applyEvent(e);
    }
  };
  socket.onclose = () => {
    bot.online = false;
    if (stopped) return;
    setTimeout(connect, retry);
    retry = Math.min(retry * 2, 15000);
  };
}

// Start straight away when a token from this tab's earlier visit is still around.
if (bot.token) login(bot.token);
