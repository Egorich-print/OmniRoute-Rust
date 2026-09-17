<script>
  import { getJson } from "../api.js";

  let { key } = $props();
  let connections = $state(null);
  let error = $state("");
  let onlyProblems = $state(false);

  $effect(() => {
    if (!key) return;
    let cancelled = false;
    getJson("/api/connections", key)
      .then((payload) => {
        if (!cancelled) {
          connections = payload.connections;
          error = payload.connection_error ?? "";
        }
      })
      .catch((err) => {
        if (!cancelled) error = err.message;
      });
    return () => {
      cancelled = true;
    };
  });

  const shown = $derived(
    (connections ?? []).filter((row) => {
      if (!onlyProblems) return true;
      if (!row.is_active) return true;
      if (!row.has_credential) return true;
      if (!row.servable) return true;
      return Boolean(row.last_error);
    }),
  );

  /// Secrets are never returned; only whether one exists.
  function credentialLabel(row) {
    if (row.has_credential) return row.auth_type === "oauth" ? "token" : "key";
    return "missing";
  }
</script>

<h1>Providers</h1>
<p class="sub">
  Connections from the shared database. <strong>servable</strong> means a backend is built for that
  provider, which requires an active row with a credential and an executor for its format.
</p>

{#if error}<div class="error">{error}</div>{/if}

<div class="toolbar">
  <label><input type="checkbox" bind:checked={onlyProblems} /> only problems</label>
</div>

{#if connections}
  <table>
    <thead>
      <tr>
        <th>provider</th><th>name</th><th>auth</th><th>credential</th>
        <th>active</th><th>servable</th><th>expires</th><th>last error</th>
      </tr>
    </thead>
    <tbody>
      {#each shown as row}
        <tr>
          <td class="mono">{row.provider}</td>
          <td>{row.name ?? "—"}</td>
          <td class="muted">{row.auth_type ?? "—"}</td>
          <td>
            {#if row.has_credential}
              <span class="tag ok">{credentialLabel(row)}</span>
            {:else}
              <span class="tag bad">missing</span>
            {/if}
          </td>
          <td>
            {#if row.is_active}<span class="tag ok">yes</span>
            {:else}<span class="tag">no</span>{/if}
          </td>
          <td>
            {#if row.servable}<span class="tag ok">yes</span>
            {:else}<span class="tag bad">no</span>{/if}
          </td>
          <td class="mono muted">{row.expires_at ?? "—"}</td>
          <td class="muted">{row.last_error ?? "—"}</td>
        </tr>
      {/each}
    </tbody>
  </table>
  {#if shown.length === 0}
    <p class="muted">All connections look healthy.</p>
  {/if}
{:else if !error}
  <p class="muted">loading…</p>
{/if}
