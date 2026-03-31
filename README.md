# ✂️ CliReqRefiner

<div align="center">

**High-Performance Request Body Refiner for AI Coding Tools (Claude Code, Codex, etc.)**

Request Body Refining · Multi-Upstream Load Balancing · Hot Config Reload · Token Cost Reduction

[![Rust](https://img.shields.io/badge/rust-1.85+-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-GPLv3-blue.svg)](LICENSE)

[![Cross-platform](https://img.shields.io/badge/platform-Windows%20%7C%20Linux%20%7C%20macOS-lightgrey.svg)](https://github.com/rust-lang/rust)

</div>

---

## 📖 About

**CliReqRefiner** is a high-performance API proxy designed for AI coding tools (Claude Code, Codex, etc.), with a focus on **refining request bodies** to reduce token consumption.

It helps you:
- ✂️ **Refine request bodies** — remove redundant system prompts, trim tool definitions, optimize context structure
- 🌐 **Connect multiple upstream providers** with automatic load balancing
- 💰 **Reduce API costs** — smart interception of non-essential requests plus request body optimization
- ⚡ **Speed up responses** — locally handle certain optimization requests
- 🔧 **Zero-downtime config** — changes take effect immediately

### 💡 Why do you need it?

AI coding tools (Claude Code, Codex, etc.) send "probing" requests during usage (e.g., quota checks, title generation, suggestion mode, etc.) that consume tokens but contribute little to actual development. CliReqRefiner smartly identifies and intercepts these requests, returning local mock responses directly — keeping the tools fully functional while significantly reducing token consumption.

Furthermore, CliReqRefiner **refines system prompts and tool definitions** to further reduce token usage. In practice, requests sent by AI coding tools often contain a large number of preset system prompts and tool definitions that are repeated in every request, consuming a significant amount of tokens. CliReqRefiner can:
- 📝 **Remove redundant system prompts**, keeping only core instructions
- 🔧 **Trim tool definitions**, keeping only essential tool descriptions
- 📊 **Optimize context structure**, reducing duplicate information

This dual optimization strategy allows CliReqRefiner to minimize API call costs while maintaining full functionality.

---

## ✨ Features

### ✂️ Request Body Refining (Core)

The core feature of CliReqRefiner — refining request bodies before forwarding to upstream:
- 📝 **System prompt optimization** — remove redundant system prompts, keep core instructions only
- 🔧 **Tool definition trimming** — slim down verbose tool descriptions
- 📊 **Context structure optimization** — reduce duplicate information across requests
- 🎯 **Granular control** — enable/disable each optimization independently via config

### 🔥 Hot Config Reload

- **Auto hot reload** when config file changes — no restart needed
- Cross-platform file watching via `notify` crate
- Smooth config switching without service interruption

### ⚡ Local Optimization Interception

Smart identification and local handling of specific requests to reduce upstream calls:

| Optimization | Description |
|:-------------|:------------|
| 🔍 **Quota check interception** | Return local mock response for quota probe requests |
| 📝 **Fast prefix detection** | Identify and extract command prefixes (e.g., `git commit`) |
| 📋 **Title generation skip** | Return default response for title generation requests |
| 💡 **Suggestion mode skip** | Return empty response for suggestion mode requests |
| 📂 **File path extraction** | Extract file paths from command output |
| 📊 **Historical analysis skip** | Return simplified response for history analysis requests |

### 🔄 Multi-Upstream Load Balancing

- Support multiple upstream service providers
- **Dual-layer round-robin**: round-robin between upstreams, then round-robin between API keys within each upstream
- Automatic API key rotation for maximum request distribution

### 📊 Request Statistics & Monitoring

- Real-time request count and token consumption statistics
- Distinguish between user input tokens, context tokens, and assistant response tokens
- Calculate token waste ratio to help optimize usage costs

---

## 🚀 Quick Start

### 🎯 Configure Claude Code CLI

Set the API endpoint in your Claude Code CLI config:

```bash
# Method 1: Environment variable
export ANTHROPIC_BASE_URL="http://127.0.0.1:9066/claude"
```

Or in `~/.claude/settings.json`:

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "http://127.0.0.1:9066/claude",
    "ANTHROPIC_AUTH_TOKEN": "anything"
  }
}
```

Where:
- `ANTHROPIC_BASE_URL` should point to `http://127.0.0.1:9066/claude`
- `ANTHROPIC_AUTH_TOKEN` can be set to anything — CliReqRefiner overrides it when forwarding

### 📦 Build

```bash
# Debug mode
sh build_native_stable.sh

# Release mode (recommended for production)
sh build_native_stable.sh r
```

### ⚙️ Configuration

Edit `config.toml`:

```toml
# Load balancing configuration example
# You can configure multiple upstreams with multiple API keys each
# Strategy: round-robin between upstreams, then round-robin between API keys
# Changes take effect immediately

# Whether to print request body
log_req_body = false
# Whether to print response body
log_res_body = false

# Upstream 1
[[upstream]]
endpoint = "https://open.bigmodel.cn/api/anthropic"
model = "glm-4.7"
api_keys = ["your_api_key1", "your_api_key2"]
# mode defaults to "anthropic" — pass through Anthropic format directly
# For OpenAI Responses format, set mode = "openai_responses"

# Upstream 2: add more upstreams for load balancing
# [[upstream]]
# endpoint = "https://another-provider.com/api/anthropic"
# model = "claude-3-5-sonnet-20241022"
# api_keys = ["your_key"]
# mode = "anthropic"  # Options: "anthropic" | "openai_responses" | "openai_chat"

[optimizations]
enable_network_probe_mock = true
enable_fast_prefix_detection = true
enable_historical_analysis_mock = true
enable_title_generation_skip = true
enable_suggestion_mode_skip = true
enable_filepath_extraction_mock = true

```

### ▶️ Run

```bash
# Use default config (config.toml)
cargo r

# Specify config file
cargo r /path/to/config.toml
```

Service listens on `0.0.0.0:9066` by default.

---

## 📖 Configuration Reference

### 🔌 Upstream Config

| Field | Type | Description |
|:------|:-----|:------------|
| `endpoint` | `String` | Upstream API address |
| `model` | `String` | Model name to enforce |
| `api_keys` | `Vec<String>` | API key list — supports multiple keys for load balancing |

### ⚙️ Optimizations Config

| Field | Type | Default | Description |
|:------|:-----|:--------|:------------|
| `enable_network_probe_mock` | `bool` | `true` | Intercept quota probe requests |
| `enable_fast_prefix_detection` | `bool` | `true` | Fast prefix detection optimization |
| `enable_historical_analysis_mock` | `bool` | `true` | Skip historical analysis requests |
| `enable_title_generation_skip` | `bool` | `true` | Skip title generation requests |
| `enable_suggestion_mode_skip` | `bool` | `true` | Skip suggestion mode requests |
| `enable_filepath_extraction_mock` | `bool` | `true` | File path extraction optimization |

---

## 🏗️ How It Works

```
┌─────────────┐     ┌──────────────┐     ┌─────────────────┐
│   Client    │────▶│CliReqRefiner │────▶│  Upstream 1     │
│             │     │              │     │  (API Key 1)    │
│             │     │  ✂️ Request  │     │  (API Key 2)    │
│             │     │    Refining  │     ├─────────────────┤
│             │     │  🔄 Load     │────▶│  Upstream 2     │
│             │     │    Balance   │     │  (API Key 1)    │
│             │     │  ⚡ Local    │     │  ...            │
│             │     │    Optimize  │     │                 │
│             │     │  📊 Token    │     │                 │
│             │     │    Stats     │     │                 │
└─────────────┘     └──────────────┘     └─────────────────┘
```

---

## 🛠️ Tech Stack

| Tech | Description |
|:-----|:------------|
| **[Salvo](https://salvo.rs/)** | High-performance async web framework |
| **[Hyper](https://hyper.rs/)** | Mature HTTP/1.1 & HTTP/2 implementation |
| **[Tokio](https://tokio.rs/)** | Rust async runtime core |
| **[arc-swap](https://docs.rs/arc-swap/)** | Lock-free hot config reload |
| **[notify](https://docs.rs/notify/)** | Cross-platform file watching |
| **[mimalloc](https://github.com/microsoft/mimalloc)** | High-performance memory allocator |

---

## ⚡ Performance

- ✅ Release build with LTO (Link Time Optimization)
- ✅ mimalloc replacing default allocator
- ✅ HTTP connection reuse, reduced connection overhead
- ✅ Lock-free config updates, no lock contention

---

## 🏘️ Community

This project was originally shared on [LINUX DO](https://linux.do/) — 如果你是社区的朋友，欢迎来帖子下交流反馈，也期待有感兴趣的开发者一起完善这个项目 🤝

---

## 📄 License

GNU General Public License v3.0 (GPLv3)
