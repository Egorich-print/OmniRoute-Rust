<script>
  const GATEWAY = import.meta.env.VITE_GATEWAY_URL ?? "http://127.0.0.1:20129";

  let status = $state("checking…");
  let prompt = $state("Write a Rust function that checks if a number is prime.");
  let answer = $state("");
  let busy = $state(false);

  async function checkHealth() {
    try {
      const res = await fetch(`${GATEWAY}/healthz`);
      const data = await res.json();
      status = `gateway ${data.status} · backend ${data.backend}`;
    } catch {
      status = "gateway unreachable";
    }
  }

  async function send() {
    busy = true;
    answer = "";
    try {
      const res = await fetch(`${GATEWAY}/v1/chat/completions`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          model: "grok-4.6",
          messages: [{ role: "user", content: prompt }],
        }),
      });
      const data = await res.json();
      answer = data.choices?.[0]?.message?.content ?? JSON.stringify(data);
    } catch (error) {
      answer = `error: ${error}`;
    } finally {
      busy = false;
    }
  }

  checkHealth();
</script>

<main>
  <header>
    <h1>OmniRoute</h1>
    <span class="status" class:down={status.includes("unreachable")}>{status}</span>
  </header>

  <label for="prompt">Prompt</label>
  <textarea id="prompt" bind:value={prompt} rows="4"></textarea>

  <div class="actions">
    <button onclick={send} disabled={busy}>{busy ? "Sending…" : "Send"}</button>
    <button class="ghost" onclick={checkHealth}>Recheck gateway</button>
  </div>

  {#if answer}
    <pre>{answer}</pre>
  {/if}
</main>

<style>
  :global(body) {
    margin: 0;
    font-family: ui-sans-serif, system-ui, sans-serif;
    background: #0b0d12;
    color: #e6e8ee;
  }

  main {
    max-width: 46rem;
    margin: 0 auto;
    padding: 2rem 1.25rem;
    display: flex;
    flex-direction: column;
    gap: 0.85rem;
  }

  header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 1rem;
  }

  h1 {
    font-size: 1.35rem;
    margin: 0;
  }

  .status {
    font-size: 0.8rem;
    color: #7ee2a8;
  }

  .status.down {
    color: #ff8a8a;
  }

  label {
    font-size: 0.8rem;
    color: #98a2b3;
  }

  textarea {
    resize: vertical;
    padding: 0.65rem;
    border-radius: 0.5rem;
    border: 1px solid #2a2f3a;
    background: #12151c;
    color: inherit;
    font: inherit;
  }

  .actions {
    display: flex;
    gap: 0.5rem;
  }

  button {
    padding: 0.55rem 1rem;
    border-radius: 0.5rem;
    border: 1px solid #2f6bff;
    background: #2f6bff;
    color: white;
    font: inherit;
    cursor: pointer;
  }

  button:disabled {
    opacity: 0.6;
    cursor: progress;
  }

  button.ghost {
    background: transparent;
    color: #9db2ff;
  }

  pre {
    margin: 0;
    padding: 0.85rem;
    border-radius: 0.5rem;
    background: #12151c;
    border: 1px solid #2a2f3a;
    white-space: pre-wrap;
    word-break: break-word;
  }
</style>
