<script>
  import { getJson } from "../api.js";

  let { key } = $props();
  let combos = $state(null);
  let error = $state("");
  let hideHealthy = $state(false);

  $effect(() => {
    if (!key) return;
    let cancelled = false;
    getJson("/api/combos", key)
      .then((payload) => {
        if (!cancelled) {
          combos = payload.combos;
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

  const shown = $derived(
    (combos ?? []).filter(
      (combo) => !hideHealthy || combo.servable_entries !== combo.total_entries,
    ),
  );
</script>

<h1>Combos</h1>
<p class="sub">
  Each entry is checked against the router's own dispatch rule, so what you see is what the gateway
  would actually try. An entry that is not servable either has no active credential or its provider
  has no executor yet.
</p>

{#if error}<div class="error">{error}</div>{/if}

<div class="toolbar">
  <label><input type="checkbox" bind:checked={hideHealthy} /> hide fully healthy combos</label>
</div>

{#if combos}
  {#each shown as combo}
    <h2>
      {combo.name}
      <span class="tag" class:ok={combo.servable_entries === combo.total_entries} class:bad={combo.servable_entries === 0}>
        {combo.servable_entries}/{combo.total_entries} servable
      </span>
      <span class="tag">{combo.strategy}</span>
    </h2>
    <table>
      <thead>
        <tr><th>provider</th><th>model</th><th>backend</th><th>status</th></tr>
      </thead>
      <tbody>
        {#each combo.entries as entry}
          <tr>
            <td class="mono">{entry.provider}</td>
            <td class="mono">{entry.model}</td>
            <td class="mono muted">{entry.backend ?? "—"}</td>
            <td>
              {#if entry.servable}
                <span class="tag ok">servable</span>
              {:else}
                <span class="tag bad">no backend</span>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/each}
  {#if shown.length === 0}
    <p class="muted">Nothing to show.</p>
  {/if}
{:else if !error}
  <p class="muted">loading…</p>
{/if}
