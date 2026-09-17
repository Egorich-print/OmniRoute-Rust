<script>
  import { getJson } from "../api.js";

  let { key } = $props();
  let limit = $state(50);
  let rows = $state(null);
  let error = $state("");

  $effect(() => {
    if (!key) return;
    const count = limit;
    let cancelled = false;
    getJson(`/api/logs?limit=${count}`, key)
      .then((payload) => {
        if (!cancelled) {
          rows = payload.logs;
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

  const shortTime = (value) => (value ?? "").replace("T", " ").replace(/\.\d+Z?$/, "");

  const statusClass = (status) => {
    if (status === null || status === undefined) return "";
    if (status >= 500) return "bad";
    if (status >= 400) return "warn";
    return "ok";
  };
</script>

<h1>Logs</h1>
<p class="sub">Recent requests from <code>call_logs</code>. Bodies are not stored here.</p>

{#if error}<div class="error">{error}</div>{/if}

<div class="toolbar">
  <label>
    rows
    <select bind:value={limit}>
      <option value={25}>25</option>
      <option value={50}>50</option>
      <option value={100}>100</option>
      <option value={250}>250</option>
    </select>
  </label>
</div>

{#if rows}
  <table>
    <thead>
      <tr>
        <th>time</th><th>status</th><th>model</th><th>provider</th>
        <th class="num">ms</th><th class="num">in</th><th class="num">out</th><th>error</th>
      </tr>
    </thead>
    <tbody>
      {#each rows as row}
        <tr>
          <td class="mono muted">{shortTime(row.timestamp)}</td>
          <td><span class="tag {statusClass(row.status)}">{row.status ?? "—"}</span></td>
          <td class="mono">{row.model ?? row.requested_model ?? "—"}</td>
          <td class="mono muted">{row.provider ?? "—"}</td>
          <td class="num">{row.duration_ms ?? "—"}</td>
          <td class="num">{row.tokens_input ?? 0}</td>
          <td class="num">{row.tokens_output ?? 0}</td>
          <td class="muted">{row.error_summary ?? ""}</td>
        </tr>
      {/each}
    </tbody>
  </table>
  {#if rows.length === 0}
    <p class="muted">No calls recorded yet.</p>
  {/if}
{:else if !error}
  <p class="muted">loading…</p>
{/if}
