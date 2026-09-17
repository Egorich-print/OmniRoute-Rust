<script>
  import { apiBase, getJson, patchJson, postJson } from "../api.js";

  let { key } = $props();
  let connections = $state(null);
  let error = $state("");
  let notice = $state("");
  let onlyProblems = $state(false);
  let savingId = $state(null);
  let restartPending = $state(false);
  let restarting = $state(false);

  async function load() {
    try {
      const payload = await getJson("/api/connections", key);
      connections = payload.connections;
      error = payload.connection_error ?? "";
    } catch (err) {
      error = err.message;
    }
  }

  $effect(() => {
    if (!key) return;
    load();
  });

  async function save(row, patch) {
    savingId = row.id;
    error = "";
    notice = "";
    try {
      const updated = await patchJson(`/api/connections/${row.id}`, key, patch);
      if (updated.restart_required) {
        restartPending = true;
        notice =
          "Saved. Connection activity changed — restart the gateway to apply it (backends are built at startup).";
      } else {
        notice = "Saved.";
      }
      await load();
    } catch (err) {
      error = err.message;
    } finally {
      savingId = null;
    }
  }

  async function restartNow() {
    restarting = true;
    error = "";
    try {
      const before = await fetch(`${apiBase()}/healthz`).then((res) => res.json());
      await postJson("/api/restart", key, {});
      notice = "Restart requested — waiting for the gateway to come back…";
      for (let i = 0; i < 30; i++) {
        await new Promise((resolve) => setTimeout(resolve, 2000));
        try {
          const health = await fetch(`${apiBase()}/healthz`).then((res) => res.json());
          if (health.status === "ok" && health.started_unix !== before.started_unix) {
            notice = "Gateway restarted and serving.";
            restartPending = false;
            await load();
            return;
          }
        } catch {
          /* still down — keep waiting */
        }
      }
      error = "Gateway did not come back within a minute — check the service.";
    } catch (err) {
      error = err.message;
    } finally {
      restarting = false;
    }
  }

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
  Toggling activity needs a gateway restart; priority and name apply on the next read.
</p>

{#if error}<div class="error">{error}</div>{/if}
{#if notice}
  <div class="notice">
    {notice}
    {#if restartPending}
      <div class="toolbar" style="margin:10px 0 0">
        <button class="primary" disabled={restarting} onclick={restartNow}>
          {restarting ? "restarting…" : "Restart now"}
        </button>
      </div>
    {/if}
  </div>
{/if}

<div class="toolbar">
  <label><input type="checkbox" bind:checked={onlyProblems} /> only problems</label>
</div>

{#if connections}
  <table>
    <thead>
      <tr>
        <th>provider</th><th>name</th><th>auth</th><th>credential</th>
        <th>active</th><th>priority</th><th>servable</th><th>expires</th><th>last error</th>
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
            <button
              disabled={savingId === row.id}
              title={row.is_active ? "Deactivate (needs restart)" : "Activate (needs restart)"}
              onclick={() => save(row, { is_active: !row.is_active })}
            >
              {#if row.is_active}on{:else}off{/if}
            </button>
          </td>
          <td class="mono">
            <button
              disabled={savingId === row.id}
              title="Lower priority"
              onclick={() => save(row, { priority: row.priority - 1 })}
            >−</button>
            {row.priority}
            <button
              disabled={savingId === row.id}
              title="Raise priority"
              onclick={() => save(row, { priority: row.priority + 1 })}
            >+</button>
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
