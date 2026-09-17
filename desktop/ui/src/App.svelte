<script>
  import { apiBase, getJson, loadKey, saveKey } from "./api.js";
  import Overview from "./views/Overview.svelte";
  import Combos from "./views/Combos.svelte";
  import Providers from "./views/Providers.svelte";
  import Keys from "./views/Keys.svelte";
  import Usage from "./views/Usage.svelte";
  import Logs from "./views/Logs.svelte";
  import Playground from "./views/Playground.svelte";

  const TABS = [
    ["overview", "Overview", Overview],
    ["combos", "Combos", Combos],
    ["providers", "Providers", Providers],
    ["keys", "Keys", Keys],
    ["usage", "Usage", Usage],
    ["logs", "Logs", Logs],
    ["playground", "Playground", Playground],
  ];

  let tab = $state("overview");
  let key = $state(loadKey());
  let draft = $state("");
  let health = $state(null);

  // Svelte 5 renders a component held in a capitalised variable directly.
  const View = $derived(TABS.find(([id]) => id === tab)?.[2] ?? Overview);

  $effect(() => {
    let cancelled = false;
    fetch(`${apiBase()}/healthz`)
      .then((res) => res.json())
      .then((data) => {
        if (!cancelled) health = data;
      })
      .catch(() => {
        if (!cancelled) health = { status: "unreachable" };
      });
    return () => {
      cancelled = true;
    };
  });

  function unlock(event) {
    event.preventDefault();
    const value = draft.trim();
    if (!value) return;
    saveKey(value);
    key = value;
    draft = "";
  }

  function signOut() {
    saveKey("");
    key = "";
  }
</script>

{#if !key}
  <form class="login row-gap" onsubmit={unlock}>
    <div class="brand" style="padding:0 0 6px">OmniRoute<small>Rust gateway</small></div>
    <p class="sub" style="margin:0">
      Paste an API key from <code>api_keys</code>. It is kept in this browser only and sent as a
      bearer token.
    </p>
    <input type="password" bind:value={draft} placeholder="sk-…" autocomplete="off" />
    <button class="primary" type="submit" disabled={!draft.trim()}>Open dashboard</button>
  </form>
{:else}
  <div class="layout">
    <aside class="sidebar">
      <div class="brand">
        OmniRoute
        <small>{health ? `${health.backend} · ${health.status}` : "checking…"}</small>
      </div>
      {#each TABS as [id, label]}
        <button class="nav" class:active={tab === id} onclick={() => (tab = id)}>{label}</button>
      {/each}
      <div style="flex:1"></div>
      <button class="nav" onclick={signOut}>Sign out</button>
    </aside>
    <main class="content">
      <View {key} />
    </main>
  </div>
{/if}
