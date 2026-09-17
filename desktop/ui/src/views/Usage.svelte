<script>
  import { getJson } from "../api.js";

  let { key } = $props();
  let days = $state(7);
  let data = $state(null);
  let error = $state("");

  $effect(() => {
    if (!key) return;
    const window = days;
    let cancelled = false;
    getJson(`/api/usage?days=${window}&limit=10`, key)
      .then((payload) => {
        if (!cancelled) {
          data = payload;
          error = payload.error ?? "";
        }
      })
      .catch((err) => {
        if (!cancelled) error = err.message;
      });
    return () => {
      cancelled = true;
    };
  });

  const compact = (value) =>
    value >= 1_000_000
      ? `${(value / 1_000_000).toFixed(1)}M`
      : value >= 1_000
        ? `${(value / 1_000).toFixed(1)}k`
        : String(value ?? 0);

  const successRate = $derived(
    data && data.requests > 0 ? Math.round((data.successes / data.requests) * 100) : null,
  );
</script>

<h1>Usage</h1>
<p class="sub">Aggregated from <code>usage_history</code>, the table both the JS and Rust gateways write.</p>

{#if error}<div class="error">{error}</div>{/if}

<div class="toolbar">
  <label>
    window
    <select bind:value={days}>
      <option value={1}>24 hours</option>
      <option value={7}>7 days</option>
      <option value={30}>30 days</option>
      <option value={90}>90 days</option>
    </select>
  </label>
</div>

{#if data && data.requests !== undefined}
  <div class="cards">
    <div class="card">
      <div class="value">{compact(data.requests)}</div>
      <div class="label">requests</div>
    </div>
    <div class="card">
      <div class="value">{successRate === null ? "—" : `${successRate}%`}</div>
      <div class="label">success</div>
    </div>
    <div class="card">
      <div class="value">{compact(data.tokens_input)}</div>
      <div class="label">tokens in</div>
    </div>
    <div class="card">
      <div class="value">{compact(data.tokens_output)}</div>
      <div class="label">tokens out</div>
    </div>
    <div class="card">
      <div class="value">{compact(data.tokens_cache_read)}</div>
      <div class="label">cache read</div>
    </div>
  </div>

  <h2>Top providers</h2>
  <table>
    <thead><tr><th>provider</th><th class="num">requests</th><th class="num">tokens in</th><th class="num">tokens out</th></tr></thead>
    <tbody>
      {#each data.by_provider as row}
        <tr>
          <td class="mono">{row.key}</td>
          <td class="num">{compact(row.requests)}</td>
          <td class="num">{compact(row.tokens_input)}</td>
          <td class="num">{compact(row.tokens_output)}</td>
        </tr>
      {/each}
    </tbody>
  </table>

  <h2>Top models</h2>
  <table>
    <thead><tr><th>model</th><th class="num">requests</th><th class="num">tokens in</th><th class="num">tokens out</th></tr></thead>
    <tbody>
      {#each data.by_model as row}
        <tr>
          <td class="mono">{row.key}</td>
          <td class="num">{compact(row.requests)}</td>
          <td class="num">{compact(row.tokens_input)}</td>
          <td class="num">{compact(row.tokens_output)}</td>
        </tr>
      {/each}
    </tbody>
  </table>
{:else if !error}
  <p class="muted">loading…</p>
{/if}
