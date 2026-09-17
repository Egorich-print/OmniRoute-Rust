<script>
  import { getJson, patchJson } from "../api.js";

  let { key } = $props();
  let combos = $state(null);
  let error = $state("");
  let notice = $state("");
  let hideHealthy = $state(false);
  let saving = $state(false);
  let editingId = $state(null);
  let draftName = $state("");
  let draftOrder = $state(0);
  let draftHidden = $state(false);

  async function load() {
    try {
      const payload = await getJson("/api/combos", key);
      combos = payload.combos;
      error = "";
    } catch (err) {
      error = err.message;
    }
  }

  $effect(() => {
    if (!key) return;
    load();
  });

  async function save(id, patch) {
    saving = true;
    error = "";
    notice = "";
    try {
      const updated = await patchJson(`/api/combos/${id}`, key, patch);
      notice = `Saved “${updated.name}”. Routing uses the new definition immediately.`;
      editingId = null;
      await load();
    } catch (err) {
      error = err.message;
    } finally {
      saving = false;
    }
  }

  function startEdit(combo) {
    editingId = combo.id;
    draftName = combo.name;
    draftOrder = combo.sort_order;
    draftHidden = combo.is_hidden;
    notice = "";
    error = "";
  }

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
  has no executor yet. Renames, reordering and hiding apply immediately — no restart needed.
</p>

{#if error}<div class="error">{error}</div>{/if}
{#if notice}<div class="notice">{notice}</div>{/if}

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
      {#if combo.is_hidden}<span class="tag">hidden</span>{/if}
      <span class="tag muted">order {combo.sort_order}</span>
      {#if editingId === combo.id}
        <button disabled={saving} onclick={() => (editingId = null)}>cancel</button>
      {:else}
        <button disabled={saving} onclick={() => startEdit(combo)}>edit</button>
        <button
          disabled={saving}
          title={combo.is_hidden ? "Show this combo to clients" : "Hide this combo from clients"}
          onclick={() => save(combo.id, { is_hidden: !combo.is_hidden })}
        >
          {combo.is_hidden ? "unhide" : "hide"}
        </button>
      {/if}
    </h2>
    {#if editingId === combo.id}
      <div class="toolbar">
        <label>
          name
          <input bind:value={draftName} maxlength="64" />
        </label>
        <label>
          order
          <input type="number" bind:value={draftOrder} />
        </label>
        <label><input type="checkbox" bind:checked={draftHidden} /> hidden</label>
        <button
          class="primary"
          disabled={saving || !draftName.trim()}
          onclick={() =>
            save(combo.id, {
              name: draftName.trim(),
              sort_order: Number(draftOrder),
              is_hidden: draftHidden,
            })}
        >
          {saving ? "saving…" : "Save"}
        </button>
      </div>
    {/if}
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
