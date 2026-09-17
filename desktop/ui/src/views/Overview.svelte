<script>
  import { getJson } from "../api.js";

  let { key } = $props();
  let data = $state(null);
  let error = $state("");

  $effect(() => {
    if (!key) return;
    let cancelled = false;
    getJson("/api/overview", key)
      .then((payload) => {
        if (!cancelled) {
          data = payload;
          error = "";
        }
      })
      .catch((err) => {
        if (!cancelled) error = err.message;
      });
    return () => {
      cancelled = true;
    };
  });
</script>

<h1>Overview</h1>
<p class="sub">Which providers are live and what the gateway can serve right now.</p>

{#if error}<div class="error">{error}</div>{/if}

{#if data}
  <div class="cards">
    <div class="card">
      <div class="value">{data.backends.length}</div>
      <div class="label">live backends</div>
    </div>
    <div class="card">
      <div class="value">{data.counts.combos}</div>
      <div class="label">combos</div>
    </div>
    <div class="card">
      <div class="value">{data.counts.connections_active}/{data.counts.connections_total}</div>
      <div class="label">connections active</div>
    </div>
    <div class="card">
      <div class="value">{data.counts.api_keys}</div>
      <div class="label">API keys</div>
    </div>
  </div>

  <h2>Live backends</h2>
  <p class="sub">
    A provider lands here only when it has an active connection with a usable credential (or needs
    none) and an executor for its wire format.
  </p>
  <div class="toolbar">
    {#each data.backends as backend}
      <span class="tag ok">{backend}</span>
    {/each}
  </div>
{:else if !error}
  <p class="muted">loading…</p>
{/if}
