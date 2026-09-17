<script>
  import { getJson, postJson } from "../api.js";

  let { key } = $props();
  let models = $state([]);
  let model = $state("");
  let prompt = $state("Reply with exactly: PONG");
  let reply = $state("");
  let meta = $state("");
  let busy = $state(false);
  let error = $state("");

  $effect(() => {
    if (!key) return;
    let cancelled = false;
    getJson("/api/combos", key)
      .then((payload) => {
        if (cancelled) return;
        const combos = payload.combos.map((combo) => combo.name);
        models = combos;
        if (!model && combos.length) model = combos[0];
      })
      .catch((err) => {
        if (!cancelled) error = err.message;
      });
    return () => {
      cancelled = true;
    };
  });

  async function send(event) {
    event.preventDefault();
    busy = true;
    reply = "";
    meta = "";
    error = "";
    const started = performance.now();
    try {
      const payload = await postJson("/v1/chat/completions", key, {
        model,
        messages: [{ role: "user", content: prompt }],
        max_tokens: 512,
      });
      const choice = payload.choices?.[0];
      reply = choice?.message?.content ?? JSON.stringify(choice ?? payload, null, 2);
      meta = `${payload.model ?? model} · ${choice?.finish_reason ?? "?"} · ${Math.round(
        performance.now() - started,
      )} ms · ${payload.usage?.total_tokens ?? 0} tokens`;
    } catch (err) {
      error = err.message;
    } finally {
      busy = false;
    }
  }
</script>

<h1>Playground</h1>
<p class="sub">Send a real request through the gateway, exactly as a client would.</p>

{#if error}<div class="error">{error}</div>{/if}

<form class="row-gap" onsubmit={send}>
  <div class="toolbar">
    <label>
      model
      <input bind:value={model} list="known-models" placeholder="combo or provider/model" />
    </label>
    <datalist id="known-models">
      {#each models as name}<option value={name}></option>{/each}
    </datalist>
    <button class="primary" type="submit" disabled={busy || !model.trim()}>
      {busy ? "sending…" : "Send"}
    </button>
  </div>
  <textarea bind:value={prompt}></textarea>
</form>

{#if meta}
  <p class="sub" style="margin-top:14px">{meta}</p>
  <pre class="reply">{reply}</pre>
{/if}
