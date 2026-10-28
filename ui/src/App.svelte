<script>
  import { bot, login, logout, saveSettings, setPaused, sellNow } from './lib.svelte.js';
  import Chart from './Chart.svelte';

  let tokenInput = $state('');
  let form = $state({ buy: '', tip: '', delay: '' });
  let formDirty = $state(false);
  let notice = $state('');

  const snap = $derived(bot.snap);
  const positions = $derived(snap ? [...snap.positions].reverse() : []);
  const sol = (l) => (l == null ? '…' : (l / 1e9).toFixed(4));
  const short = (s) => (s.length > 12 ? `${s.slice(0, 5)}…${s.slice(-5)}` : s);
  const time = (ms) => new Date(ms).toLocaleTimeString([], { hour12: false });
  const solscan = (kind, v) => `https://solscan.io/${kind}/${v}`;

  // Keep the form in sync with the bot until the user starts typing.
  $effect(() => {
    if (snap && !formDirty) {
      form = {
        buy: String(snap.settings.buy_amount_sol),
        tip: String(snap.settings.bribe_fee_sol),
        delay: String(snap.settings.sell_delay_slots),
      };
    }
  });

  async function guarded(fn) {
    notice = '';
    try {
      await fn();
    } catch (e) {
      notice = e.message;
    }
  }

  const apply = () =>
    guarded(async () => {
      await saveSettings({
        buy_amount_sol: Number(form.buy),
        bribe_fee_sol: Number(form.tip),
        sell_delay_slots: Number(form.delay),
      });
      formDirty = false;
    });

  const statusStyle = {
    held: 'text-accent', selling: 'text-warn', sold: 'text-good', failed: 'text-bad',
  };
  const eventStyle = {
    buy_sent: 'text-good', sell_sent: 'text-good', buy_failed: 'text-bad', sell_failed: 'text-bad',
    sell_started: 'text-warn', paused: 'text-warn', stream: 'text-warn', settings_changed: 'text-accent',
    create_detected: 'text-dim', balance: 'text-dim', skipped: 'text-dim',
  };

  function describe(e) {
    switch (e.type) {
      case 'create_detected': return `create ${short(e.mint)} @ ${e.slot}`;
      case 'buy_sent': return `buy ${short(e.mint)} ${e.sol} SOL, sell at ${e.sell_at_slot}`;
      case 'skipped': return `skipped ${short(e.mint)}: ${e.reason}`;
      case 'buy_failed': return `buy failed ${short(e.mint)}: ${e.error}`;
      case 'sell_started': return `selling ${short(e.mint)}`;
      case 'sell_sent': return `sold ${short(e.mint)} (${e.amount} tokens)`;
      case 'sell_failed': return `sell failed ${short(e.mint)}: ${e.error}`;
      case 'balance': return `balance ${sol(e.lamports)} SOL`;
      case 'stream': return `stream ${e.connected ? 'connected' : 'disconnected'}`;
      case 'paused': return e.paused ? 'bot paused' : 'bot resumed';
      case 'settings_changed': return 'settings changed';
      default: return e.type;
    }
  }
</script>

{#if !snap}
  <main class="grid min-h-screen place-items-center p-4">
    <form
      class="w-full max-w-sm space-y-4 rounded-lg border border-line bg-panel p-6"
      onsubmit={(e) => { e.preventDefault(); login(tokenInput); }}
    >
      <h1 class="text-lg font-semibold">Sniper</h1>
      <p class="text-sm text-dim">Enter the API token from your <code class="num">.env</code> (or from the bot's log).</p>
      <input
        type="password"
        bind:value={tokenInput}
        autocomplete="off"
        placeholder="API_TOKEN"
        class="num w-full rounded border border-line bg-bg px-3 py-2 outline-none focus:border-accent"
      />
      {#if bot.authError}<p class="text-sm text-bad">{bot.authError}</p>{/if}
      <button class="w-full rounded bg-accent px-3 py-2 font-medium text-bg hover:opacity-90">Connect</button>
    </form>
  </main>
{:else}
  <main class="mx-auto flex min-h-screen max-w-7xl flex-col gap-4 p-4">
    <header class="flex flex-wrap items-center gap-x-6 gap-y-2 rounded-lg border border-line bg-panel px-4 py-3">
      <h1 class="font-semibold">Sniper</h1>
      <span class={snap.settings.paused ? 'text-warn' : 'text-good'}>{snap.settings.paused ? 'PAUSED' : 'RUNNING'}</span>
      <span class={snap.stream_connected ? 'text-good' : 'text-bad'}>stream {snap.stream_connected ? 'up' : 'down'}</span>
      <span class="text-dim">slot <span class="num text-ink">{snap.slot}</span></span>
      <span class="text-dim">balance <span class="num text-ink">{sol(snap.balance_lamports)} SOL</span></span>
      <span class="text-dim" title="Total SOL spent on buys this run / cap">
        spent <span class="num text-ink">{snap.session_spent_sol.toFixed(4)}{snap.risk.max_session_spend_sol > 0 ? ` / ${snap.risk.max_session_spend_sol}` : ''} SOL</span>
      </span>
      {#if snap.drawdown_sol != null}
        <span class="text-dim" title="Wallet balance below its starting value (open positions count as spent)">
          drawdown <span class="num {snap.risk.max_session_drawdown_sol > 0 && snap.drawdown_sol >= snap.risk.max_session_drawdown_sol ? 'text-bad' : 'text-ink'}">{snap.drawdown_sol.toFixed(4)} SOL</span>
        </span>
      {/if}
      <span class="text-dim">creates seen <span class="num text-ink">{bot.createsSeen}</span></span>
      <span class="num text-dim" title={snap.wallet}>{short(snap.wallet)}</span>
      <span class="ml-auto flex items-center gap-3">
        {#if !bot.online}<span class="text-warn">reconnecting…</span>{/if}
        <button class="rounded border border-line px-3 py-1 hover:border-warn" onclick={() => guarded(() => setPaused(!snap.settings.paused))}>
          {snap.settings.paused ? 'Resume' : 'Pause'}
        </button>
        <button class="text-dim hover:text-ink" onclick={logout}>Log out</button>
      </span>
    </header>

    {#if notice}<p class="rounded border border-bad/40 bg-bad/10 px-3 py-2 text-sm text-bad">{notice}</p>{/if}

    <div class="grid gap-4 lg:grid-cols-[1fr_20rem]">
      <section class="rounded-lg border border-line bg-panel p-4">
        <h2 class="mb-2 text-sm font-medium text-dim">Wallet balance (SOL)</h2>
        <div class="h-48">
          <Chart t={bot.balance.t} v={bot.balance.v} />
        </div>
      </section>

      <form class="space-y-3 rounded-lg border border-line bg-panel p-4" onsubmit={(e) => { e.preventDefault(); apply(); }}>
        <h2 class="text-sm font-medium text-dim">Settings (applied to the next buy)</h2>
        {#each [['buy', 'Buy amount (SOL)'], ['tip', 'Landing tip (SOL)'], ['delay', 'Sell after (slots)']] as [key, label]}
          <label class="block text-sm">
            <span class="text-dim">{label}</span>
            <input
              bind:value={form[key]}
              oninput={() => (formDirty = true)}
              inputmode="decimal"
              class="num mt-1 w-full rounded border border-line bg-bg px-3 py-1.5 outline-none focus:border-accent"
            />
          </label>
        {/each}
        <button class="w-full rounded bg-accent px-3 py-2 font-medium text-bg hover:opacity-90 disabled:opacity-40" disabled={!formDirty}>
          Apply
        </button>
      </form>
    </div>

    <section class="rounded-lg border border-line bg-panel p-4 text-sm">
      <h2 class="mb-2 font-medium text-dim">Risk limits (set in .env, 0 = off)</h2>
      <dl class="num grid grid-cols-2 gap-x-6 gap-y-1 sm:grid-cols-3 lg:grid-cols-5">
        {#each [
          ['max open positions', snap.risk.max_open_positions],
          ['session spend cap', `${snap.risk.max_session_spend_sol} SOL`],
          ['drawdown cap', `${snap.risk.max_session_drawdown_sol} SOL`],
          ['buy slippage', `${snap.risk.buy_slippage_percent}%`],
          ['dev buy range', `${snap.risk.min_dev_buy_sol}..${snap.risk.max_dev_buy_sol} SOL`],
          ['balance reserve', `${snap.risk.min_balance_reserve_sol} SOL`],
          ['blocked creators', snap.risk.blocked_creators],
          ['blocked keywords', snap.risk.blocked_keywords.join(', ') || '-'],
        ] as [label, value]}
          <div><dt class="font-sans text-dim">{label}</dt><dd>{value}</dd></div>
        {/each}
      </dl>
    </section>

    <section class="overflow-x-auto rounded-lg border border-line bg-panel">
      <h2 class="px-4 pt-4 text-sm font-medium text-dim">Positions</h2>
      <table class="w-full text-left text-sm">
        <thead class="text-dim">
          <tr><th class="px-4 py-2">Mint</th><th>Status</th><th>SOL</th><th>Slots left</th><th>Opened</th><th>Note</th><th></th></tr>
        </thead>
        <tbody>
          {#each positions as p (p.mint)}
            <tr class="border-t border-line">
              <td class="num px-4 py-2"><a class="hover:text-accent" href={solscan('token', p.mint)} target="_blank" rel="noreferrer">{short(p.mint)}</a></td>
              <td class={statusStyle[p.status]}>{p.status}</td>
              <td class="num">{p.sol}</td>
              <td class="num">{p.status === 'held' ? Math.max(0, p.sell_at_slot - snap.slot) : '-'}</td>
              <td class="num text-dim">{time(p.opened_ms)}</td>
              <td class="max-w-xs truncate text-bad" title={p.error ?? ''}>{p.error ?? ''}</td>
              <td class="pr-4 text-right">
                {#if p.status === 'held' || p.status === 'failed'}
                  <button class="rounded border border-line px-2 py-0.5 hover:border-warn" onclick={() => guarded(() => sellNow(p.mint))}>
                    {p.status === 'failed' ? 'Retry sell' : 'Sell now'}
                  </button>
                {/if}
              </td>
            </tr>
          {:else}
            <tr><td class="px-4 py-6 text-dim" colspan="7">No positions yet.</td></tr>
          {/each}
        </tbody>
      </table>
    </section>

    <section class="rounded-lg border border-line bg-panel p-4">
      <h2 class="mb-2 text-sm font-medium text-dim">Events</h2>
      <ul class="num max-h-80 space-y-0.5 overflow-y-auto text-sm">
        {#each bot.events as e, i (e.ts_ms + ':' + e.type + ':' + i)}
          <li class="flex gap-3">
            <span class="text-dim">{time(e.ts_ms)}</span>
            <span class={eventStyle[e.type] ?? ''}>{describe(e)}</span>
          </li>
        {/each}
      </ul>
    </section>
  </main>
{/if}
