# STATUS

> OmniRoute-Rust — Rust-шлюз, дашборд и десктоп-оболочка.
> Обновлено: 2026-09-18

## Identity

```yaml
Name: OmniRoute-Rust
Type: Platform / Service (AI gateway)
Layer: Intelligence / Execution
Status: Active
Repository: ai-workstation/Projects/OmniRoute-Rust
GitHub: https://github.com/Egorich-print/OmniRoute-Rust
```

## Что работает

- **Шлюз** (`omniroute-gateway`, axum): OpenAI-совместимые `/v1/chat/completions`
  и `/v1/models`, стриминг SSE, роутинг по комбо из БД, breaker-классификация
  (4xx не роняет провайдера), failover по кандидатам.
- **Бэкенды:** `openai`-совместимые, `gemini` (с персистентными
  thought signatures), `openai-responses` (deepseek), keyless-провайдеры,
  `grok-cli`. 15 живых бэкендов на текущей БД.
- **Дашборд** (Svelte 5): Overview, Combos (редактирование, включая список
  моделей/веса/стратегию), Providers (активность/приоритет), Keys
  (выпуск/отзыв), Usage, Logs, Playground.
- **Admin-API:** `/api/overview`, `/api/combos`, `/api/connections`, `/api/keys`,
  `/api/usage`, `/api/logs`, `/api/restart`. Мутации — см. ADR-004.
- **Десктоп** (Tauri v2): тот же бандл + sidecar шлюза на `:20129`.
- **Сервис:** launchd `com.omniroute.rust`, порт `20128`, UI из
  `desktop/ui/dist`.

## Тесты

```
cargo test --workspace    ~100 тестов, все зелёные
cargo clippy --workspace --all-targets -- -D warnings    чисто
```

## Состояние миграции

JS-установка удалена 2026-09-18 (см. `Archives/OmniRoute/`). Rust-реализация —
единственный рабочий рантайм. Осознанные разрывы с JS-дашбордом:

- `/api/mcp/*` не портирован;
- OAuth-перелогин провайдеров требует временной переустановки JS;
- нет части служебных страниц JS (биллинг, телеграм, геймификация и т.п.).

## Ближайшие шаги

1. Single-instance guard для десктопа (второй инстанс упирается в занятый
   порт sidecar'а).
2. Довести остаток паритета по мере надобности, а не «на всякий случай».
3. Почистить `.build/` (5 ГБ) в репозитории, если не нужен.

## Документы

| Что               | Где                                       |
| ----------------- | ----------------------------------------- |
| Обзор и связи     | `Obsidian Vault/Projects/OmniRoute-Rust/` |
| ADR               | `docs/adr/`                               |
| История работ     | `docs/history/`                           |
| Архивы и откат JS | `~/ai-workstation/Archives/OmniRoute/`    |
