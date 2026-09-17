<script>
  import { getJson, postJson } from "../api.js";

  let { key } = $props();
  let rows = $state(null);
  let error = $state("");
  let notice = $state("");
  let busy = $state(false);
  let draftName = $state("");
  /// The freshly issued secret — shown once, never refetched.
  let freshSecret = $state(null);

  async function load() {
    try {
      const payload = await getJson("/api/keys", key);
      rows = payload.keys;
      error = payload.error ?? "";
    } catch (err) {
      error = err.message;
    }
  }

  $effect(() => {
    if (!key) return;
    load();
  });

  async function create(event) {
    event.preventDefault();
    busy = true;
    error = "";
    notice = "";
    try {
      const created = await postJson("/api/keys", key, { name: draftName.trim() });
      freshSecret = created;
      draftName = "";
      await load();
    } catch (err) {
      error = err.message;
    } finally {
      busy = false;
    }
  }

  async function setRevoked(row, revoked) {
    if (revoked && !confirm(`Revoke key “${row.name}”? Clients using it get 401 immediately.`)) {
      return;
    }
    busy = true;
    error = "";
    notice = "";
    freshSecret = null;
    try {
      const updated = await postJson(
        `/api/keys/${row.id}/${revoked ? "revoke" : "restore"}`,
        key,
        {},
      );
      notice = revoked
        ? `Key revoked at ${updated.revoked_at ?? "now"}. It already stopped working.`
        : "Key restored.";
      await load();
    } catch (err) {
      error = err.message;
    } finally {
      busy = false;
    }
  }

  async function copySecret() {
    try {
      await navigator.clipboard.writeText(freshSecret.key);
      notice = "Copied to clipboard.";
    } catch {
      error = "Clipboard blocked — select the key and copy it manually.";
    }
  }

  const shortTime = (value) => (value ?? "").replace("T", " ").replace(/\.\d+Z?$/, "");
</script>

<h1>Keys</h1>
<p class="sub">
  Operator keys for this gateway. Only prefixes are listed — the full secret is shown
  <strong>once</strong>, right after creation. Revocation applies immediately, no restart needed.
</p>

{#if error}<div class="error">{error}</div>{/if}
{#if notice}<div class="notice">{notice}</div>{/if}

{#if freshSecret}
  <div class="notice">
    <div>New key <strong>{freshSecret.name}</strong> (<code>{freshSecret.prefix}…</code>). {freshSecret.warning}</div>
    <pre class="reply">{freshSecret.key}</pre>
    <div class="toolbar" style="margin:12px 0 0">
      <button class="primary" onclick={copySecret}>Copy</button>
      <button onclick={() => (freshSecret = null)}>I stored it</button>
    </div>
  </div>
{/if}

<form class="toolbar" onsubmit={create}>
  <label>
    new key name
    <input bind:value={draftName} maxlength="64" placeholder="e.g. laptop" />
  </label>
  <button class="primary" type="submit" disabled={busy || !draftName.trim()}>Issue key</button>
</form>

{#if rows}
  <table>
    <thead>
      <tr><th>name</th><th>prefix</th><th>status</th><th>created</th><th>last used</th><th></th></tr>
    </thead>
    <tbody>
      {#each rows as row}
        <tr>
          <td>{row.name}</td>
          <td class="mono">{row.encrypted ? "enc:v1…" : `${row.prefix ?? "…"}…`}</td>
          <td>
            {#if row.revoked}<span class="tag bad">revoked</span>
            {:else if row.encrypted}<span class="tag warn">JS-only</span>
            {:else if !row.is_active}<span class="tag">off</span>
            {:else}<span class="tag ok">live</span>{/if}
          </td>
          <td class="mono muted">{shortTime(row.created_at) || "—"}</td>
          <td class="mono muted">{shortTime(row.last_used_at) || "—"}</td>
          <td>
            {#if !row.encrypted}
              {#if row.revoked}
                <button disabled={busy} onclick={() => setRevoked(row, false)}>restore</button>
              {:else}
                <button disabled={busy} onclick={() => setRevoked(row, true)}>revoke</button>
              {/if}
            {/if}
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
  {#if rows.length === 0}
    <p class="muted">No keys yet — issue the first one above.</p>
  {/if}
{:else if !error}
  <p class="muted">loading…</p>
{/if}
