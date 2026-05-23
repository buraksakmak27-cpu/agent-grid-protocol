# ⚡ Agent Grid Protocol

> **Stop wasting your idle AI Agent API quotas. Earn passive income or bypass rate-limits with <1ms Rust in-memory order book.**

A high-performance AI API quota exchange built in Rust with Axum. Agents sell unused API quotas, other agents buy them — matched in microseconds via a `BTreeMap`-backed order book protected by `Arc<RwLock<_>>`.

---

## Why Agent Grid?

| Problem | Agent Grid Solution |
|---|---|
| You hit OpenAI / Anthropic rate limits | Buy quota from agents with spare capacity |
| Your API quota sits idle at night | Sell it, earn passive income |
| You pay full price for every token | The market sets the price — always cheapest first |
| Complex integration | Drop-in OpenAI & Anthropic SDK compatible proxy |

---

## Architecture

```
  ┌─────────────────────────────────────────────────────┐
  │              Agent Grid — Rust Core                 │
  │                                                     │
  │  BTreeMap<OrderedFloat<f64>, Vec<Order>>            │
  │  ← sorted asks, <1ms match latency                 │
  │                                                     │
  │  POST /order               ← sell quota             │
  │  GET  /book                ← browse market          │
  │  POST /v1/chat/completions ← OpenAI proxy           │
  │  POST /v1/messages         ← Anthropic proxy        │
  └─────────────────────────────────────────────────────┘
```

---

## Quick Start

### 1. Start the exchange

```bash
cargo run
```

```
╔══════════════════════════════════════════════════════╗
║        TOKEN BORSASI — Agent Grid  v0.1.0            ║
╚══════════════════════════════════════════════════════╝

         POST  /order
         GET   /book
         POST  /v1/chat/completions   (OpenAI)
         POST  /v1/messages           (Anthropic Claude)
```

### 2. Use the Python SDK

```bash
pip install requests
```

**OpenAI — drop-in replacement:**

```python
from agent_grid_client import AgentGrid

grid = AgentGrid()
response = grid.chat_completion(
    model="gpt-4o",
    messages=[{"role": "user", "content": "Hello from Agent Grid!"}]
)
print(response["choices"][0]["message"]["content"])
```

**Anthropic Claude — fully compatible:**

```python
from agent_grid_client import AgentGrid

grid = AgentGrid()
response = grid.claude_completion(
    model="claude-3-5-sonnet",
    messages=[{"role": "user", "content": "Hello from Agent Grid!"}]
)
print(response["content"][0]["text"])
```

---

## API Reference

### `POST /order` — List quota for sale

```json
{
  "provider": "OpenAI",
  "model": "gpt-4o",
  "token_amount": 50000,
  "price_per_1k": 0.0015
}
```

### `GET /book` — Browse the order book

```json
{
  "total_orders": 42,
  "asks": [
    { "id": 7, "provider": "OpenAI", "model": "gpt-4o", "token_amount": 49000, "price_per_1k": 0.0014 }
  ]
}
```

### `POST /v1/chat/completions` — OpenAI-compatible proxy

Standard OpenAI SDK request. The exchange matches the cheapest `gpt-4o` quota automatically.

### `POST /v1/messages` — Anthropic-compatible proxy

Standard Anthropic SDK request. The exchange matches the cheapest Claude quota automatically.

---

## Python SDK Methods

| Method | Description |
|---|---|
| `chat_completion(model, messages)` | OpenAI-format completion via the exchange |
| `claude_completion(model, messages)` | Anthropic-format completion via the exchange |
| `cheapest_message(model, content)` | One-liner OpenAI response as string |
| `ask_claude(content)` | One-liner Claude response as string |
| `list_orders()` | Fetch the live order book |
| `add_order(provider, model, tokens, price)` | List your quota for sale |

---

## Environment Variables

| Variable | Description |
|---|---|
| `OPENAI_API_KEY` | If set, requests are tunnelled to real OpenAI API |
| `ANTHROPIC_API_KEY` | If set, requests are tunnelled to real Anthropic API |

If neither is set, the exchange returns realistic mock responses — perfect for development and load testing.

---

## Market Maker Simulation

The exchange ships with a built-in autonomous market maker that generates realistic order flow every 2–4 seconds:

```
[BOT HACMİ] Yeni Kota Eklendi | #  12 | OpenAI     gpt-4o        | 23000 token | $0.001649/1k
[BOT HACMİ] Yeni Kota Eklendi | #  13 | Anthropic  claude-3-5-sonnet | 41000 token | $0.003306/1k
```

---

## Tech Stack

- **Rust** + **Axum** 0.7 — async HTTP server
- **BTreeMap** + **ordered-float** — sub-millisecond price-sorted order matching
- **Arc\<RwLock\<_\>>** — lock-free concurrent state
- **reqwest** — upstream API tunnelling
- **tower-http** — CORS middleware

---

## License

MIT © Agent Grid Protocol
