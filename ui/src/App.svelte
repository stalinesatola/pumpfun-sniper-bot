<script>
  import {
    bot, login, logout, saveSettings, setPaused, sellNow, setMode,
    setCopyEnabled, addCopyWallet, removeCopyWallet,
  } from './lib.svelte.js';
  import Chart from './Chart.svelte';

  let tokenInput = $state('');
  let form = $state({ buy: '', tip: '', delay: '' });
  let formDirty = $state(false);
  let notice = $state('');
  let newWallet = $state('');

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

  const modes = [
    ['buy_only', 'Buy only', 'Opens positions, never sells by itself'],
    ['buy_and_sell', 'Buy & sell', 'Normal operation'],
    ['sell_only', 'Sell only', 'Opens nothing new, only sells what is held'],
  ];

  const addWallet = () =>
    guarded(async () => {
      await addCopyWallet(newWallet);
      newWallet = '';
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
    create_detected: 'text-dim', balance: 'text-dim', skipped: 'text-dim', copy_signal: 'text-accent', copy_changed: 'text-accent',
  };

  function describe(e) {
    switch (e.type) {
      case 'create_detected': return `create ${short(e.mint)} @ ${e.slot}`;
      case 'buy_sent': return `buy ${short(e.mint)} ${e.sol} SOL, sell at ${e.sell_at_slot}`;
      case 'copy_signal': return `leader ${short(e.leader)} ${e.side === 'buy' ? 'bought' : 'sold'} ${short(e.mint)} (${e.sol.toFixed(3)} SOL)`;
      case 'copy_changed': return `copy-trade ${e.copy.enabled ? 'on' : 'off'}, ${e.copy.wallets.length} wallet(s) followed`;
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
      <div class="flex overflow-hidden rounded border border-line" role="group" aria-label="Trading mode">
        {#each modes as [value, label, hint]}
          <button
            title={hint}
            class="px-3 py-1 text-sm {snap.settings.mode === value ? 'bg-accent font-medium text-bg' : 'text-dim hover:text-ink'}"
            onclick={() => guarded(() => setMode(value))}
          >
            {label}
          </button>
        {/each}
      </div>
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
        <p class="text-xs {snap.settings.cost_pct_of_buy > 20 ? 'text-bad' : 'text-dim'}">
          Fees for one buy + sell: about {snap.settings.round_trip_cost_sol.toFixed(5)} SOL
          ({snap.settings.cost_pct_of_buy.toFixed(0)}% of the buy).
          {#if snap.settings.cost_pct_of_buy > 20}
            A trade must gain more than that just to break even: raise the buy amount or lower the tip.
          {/if}
        </p>
        {#if snap.settings.save_error}
          <p class="text-xs text-bad">Mode is active but {snap.settings.save_error}.</p>
        {/if}
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
      <h2 class="mb-2 mt-4 font-medium text-dim">
        Buy rule: new coins only{snap.community.require_community ? ', with an active community' : ' (community check off)'}
      </h2>
      <dl class="num grid grid-cols-2 gap-x-6 gap-y-1 sm:grid-cols-3 lg:grid-cols-5">
        {#each [
          ['watching now', snap.community.watching],
          ['decide after', snap.community.require_community ? `${snap.community.window_slots} slots` : 'at creation'],
          ['unique buyers', snap.community.require_community ? `>= ${snap.community.min_buyers}` : '-'],
          ['SOL bid', snap.community.require_community ? `>= ${snap.community.min_volume_sol}` : '-'],
          ['copy: max token age', snap.community.max_token_age_slots ? `${snap.community.max_token_age_slots} slots` : 'no limit'],
        ] as [label, value]}
          <div><dt class="font-sans text-dim">{label}</dt><dd>{value}</dd></div>
        {/each}
      </dl>
    </section>

    <section class="rounded-lg border border-line bg-panel p-4 text-sm">
      <div class="mb-3 flex flex-wrap items-center gap-3">
        <h2 class="font-medium text-dim">Copy-trade</h2>
        <span class={snap.copy.active ? 'text-good' : 'text-dim'}>
          {snap.copy.active ? 'ACTIVE' : snap.copy.enabled ? 'on, but no wallets followed' : 'OFF'}
        </span>
        <button
          class="ml-auto rounded border px-3 py-1 {snap.copy.enabled ? 'border-line hover:border-warn' : 'border-accent text-accent hover:opacity-80'}"
          onclick={() => guarded(() => setCopyEnabled(!snap.copy.enabled))}
        >
          {snap.copy.enabled ? 'Turn off' : 'Turn on'}
        </button>
      </div>

      <p class="mb-3 text-dim">
        Mirrors the Pump.fun buys of the wallets below, only for new tokens that pass the buy rule.
        Buy {snap.copy.buy_sol} SOL per copy,
        {snap.copy.sell_with_leader ? 'sells when the leader sells' : 'sells on a timer only'},
        max hold {snap.copy.max_hold_slots} slots{#if snap.copy.min_leader_sol || snap.copy.max_leader_sol}, leader size {snap.copy.min_leader_sol}..{snap.copy.max_leader_sol} SOL{/if}.
        Turning it off keeps the list; copies already held still sell on the timer.
      </p>

      <form class="mb-3 flex gap-2" onsubmit={(e) => { e.preventDefault(); addWallet(); }}>
        <input
          bind:value={newWallet}
          placeholder="Wallet address to follow"
          autocomplete="off"
          spellcheck="false"
          class="num min-w-0 flex-1 rounded border border-line bg-bg px-3 py-1.5 outline-none focus:border-accent"
        />
        <button class="rounded bg-accent px-4 py-1.5 font-medium text-bg hover:opacity-90 disabled:opacity-40" disabled={!newWallet.trim()}>
          Add
        </button>
      </form>

      <ul class="space-y-1">
        {#each snap.copy.wallets as w (w)}
          <li class="flex items-center gap-3 rounded border border-line px-3 py-1.5">
            <a class="num min-w-0 flex-1 truncate hover:text-accent" href={solscan('account', w)} target="_blank" rel="noreferrer" title={w}>{w}</a>
            <button class="text-dim hover:text-bad" onclick={() => guarded(() => removeCopyWallet(w))}>Remove</button>
          </li>
        {:else}
          <li class="text-dim">No wallets followed yet.</li>
        {/each}
      </ul>

      <p class="mt-3 text-xs text-dim">
        {#if snap.copy.save_error}
          <span class="text-bad">The change is active but {snap.copy.save_error}.</span>
        {:else if snap.copy.persisted}
          Saved to disk: the list and the switch survive a restart and replace COPY_WALLETS from .env.
        {:else}
          Not saved to disk (COPY_STATE_FILE=off): changes are lost when the bot restarts.
        {/if}
      </p>
    </section>

    <section class="overflow-x-auto rounded-lg border border-line bg-panel">
      <h2 class="px-4 pt-4 text-sm font-medium text-dim">Positions</h2>
      <table class="w-full text-left text-sm">
        <thead class="text-dim">
          <tr><th class="px-4 py-2">Mint</th><th>Source</th><th>Status</th><th>SOL</th><th>Slots left</th><th>Opened</th><th>Note</th><th></th></tr>
        </thead>
        <tbody>
          {#each positions as p (p.mint)}
            <tr class="border-t border-line">
              <td class="num px-4 py-2"><a class="hover:text-accent" href={solscan('token', p.mint)} target="_blank" rel="noreferrer">{short(p.mint)}</a></td>
              <td class={p.source?.kind === 'copy' ? 'text-accent' : 'text-dim'} title={p.source?.leader ?? ''}>
                {p.source?.kind === 'copy' ? `copy ${short(p.source.leader)}` : 'snipe'}
              </td>
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
            <tr><td class="px-4 py-6 text-dim" colspan="8">No positions yet.</td></tr>
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
