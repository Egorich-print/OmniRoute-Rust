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
  let draftStrategy = $state("priority");
  let draftModels = $state([]);
  let addProvider = $state("");
  let addModel = $state("");
  let addWeight = $state(50);

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
    draftStrategy = combo.strategy || "priority";
    draftModels = combo.entries.map((entry) => ({
      id: entry.id ?? "",
      provider: entry.provider,
      model: entry.model,
      weight: entry.weight ?? 0,
    }));
    addProvider = "";
    addModel = "";
    addWeight = 50;
    notice = "";
    error = "";
  }

  const clampWeight = (value) => Math.min(100, Math.max(0, Number(value) || 0));

  function bumpWeight(index, delta) {
    const row = draftModels[index];
    draftModels[index] = { ...row, weight: clampWeight(row.weight + delta) };
  }

  function removeModel(index) {
    draftModels = draftModels.filter((_, j) => j !== index);
  }

  function addModelRow() {
    const provider = addProvider.trim();
    let bare = addModel.trim();
    if (!provider || !bare) return;
    // Paste convenience: "groq/openai/gpt-oss-20b" with provider "groq"
    // means the bare id "openai/gpt-oss-20b". The server joins verbatim,
    // so only the add form strips — saved rows always round-trip untouched.
    if (bare.startsWith(`${provider}/`)) bare = bare.slice(provider.length + 1);
    if (!bare) return;
    draftModels = [
      ...draftModels,
      { id: "", provider, model: bare, weight: clampWeight(addWeight) },
    ];
    addProvider = "";
    addModel = "";
    addWeight = 50;
  }

  function saveFull(combo) {
    save(combo.id, {
      name: draftName.trim(),
      sort_order: Number(draftOrder),
      is_hidden: draftHidden,
      strategy: draftStrategy,
      models: draftModels.map((row) => ({
        id: row.id || null,
        provider: row.provider,
        model: row.model,
        weight: row.weight,
      })),
    });
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
  has no executor yet. All edits — including the model list — apply immediately, no restart needed.
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
        <label>
          strategy
          <select bind:value={draftStrategy}>
            <option value="priority">priority</option>
            <option value="auto">auto</option>
          </select>
        </label>
        <label><input type="checkbox" bind:checked={draftHidden} /> hidden</label>
      </div>
      <table>
        <thead>
          <tr><th>provider / model</th><th>weight</th><th></th></tr>
        </thead>
        <tbody>
          {#each draftModels as row, i}
            <tr>
              <td class="mono">{row.provider}/{row.model}</td>
              <td class="mono">
                <button disabled={saving} title="−5" onclick={() => bumpWeight(i, -5)}>−</button>
                {row.weight}
                <button disabled={saving} title="+5" onclick={() => bumpWeight(i, 5)}>+</button>
              </td>
              <td>
                <button disabled={saving} title="Remove from combo" onclick={() => removeModel(i)}>✕</button>
              </td>
            </tr>
          {/each}
          <tr>
            <td>
              <input placeholder="provider" bind:value={addProvider} maxlength="48" />
              <input placeholder="bare model id" bind:value={addModel} maxlength="128" />
            </td>
            <td><input type="number" min="0" max="100" bind:value={addWeight} /></td>
            <td><button disabled={saving || !addProvider.trim() || !addModel.trim()} onclick={addModelRow}>add</button></td>
          </tr>
        </tbody>
      </table>
      <div class="toolbar">
        <button
          class="primary"
          disabled={saving || !draftName.trim() || draftModels.length === 0}
          onclick={() => saveFull(combo)}
        >
          {saving ? "saving…" : "Save combo"}
        </button>
        {#if draftModels.length === 0}
          <span class="muted">A combo needs at least one model.</span>
        {/if}
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
