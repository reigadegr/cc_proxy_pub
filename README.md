# 🚀 CC Proxy

<div align="center">

**专为 Claude Code CLI 打造的高性能 AI API 代理网关**

多上游负载均衡 · 智能本地优化 · 热配置重载 · 专为 Claude Code 优化

[![Rust](https://img.shields.io/badge/rust-1.85+-orange.svg)](https://www.rust-lang.org)
[![Claude](https://img.shields.io/badge/Claude-Code_CLI-purple.svg)](https://claude.com/claude-code)
[![License](https://img.shields.io/badge/license-GPLv3-blue.svg)](LICENSE)

[![Cross-platform](https://img.shields.io/badge/platform-Windows%20%7C%20Linux%20%7C%20macOS-lightgrey.svg)](https://github.com/rust-lang/rust)

</div>

---

## 📖 关于

**CC Proxy** 是专为 [Claude Code CLI](https://claude.com/claude-code) 设计的高性能 API 代理网关。

它不仅能帮你：
- 🌐 **接入多个上游服务商**，自动负载均衡
- 💰 **降低 API 成本**，智能拦截非必要请求，并通过移除不必要的系统提示词与工具定义进一步降低 Token 消耗
- ⚡ **提升响应速度**，本地处理部分优化请求
- 🔧 **零停机配置**，修改配置立即生效

### 💡 为什么需要它？

Claude Code CLI 在使用过程中会发送一些"探测性"请求（如配额检查、标题生成、建议模式等），这些请求虽然消耗 Token 但对实际开发帮助有限。CC Proxy 智能识别并拦截这些请求，直接返回本地 mock 响应，既保持了 Claude Code 的正常功能，又能显著降低 Token 消耗。

此外，CC Proxy 还能通过**精简系统提示词和工具定义**来进一步降低 Token 消耗。在实际使用中，Claude Code 发送的请求往往包含大量预设的系统提示词和工具定义，这些内容在每个请求中都会重复出现，占用大量 Token。CC Proxy 可以在转发请求时：
- 📝 **移除冗余的系统提示词**，保留核心指令
- 🔧 **精简工具定义**，仅保留必要的工具描述
- 📊 **优化上下文结构**，减少重复信息

这种双重优化策略使得 CC Proxy 能够在保持功能完整性的同时，最大程度地降低 API 调用成本。

---

## ✨ 功能特性

### 🔄 多上游负载均衡

- 支持配置多个 upstream 服务提供商
- **双层轮询策略**：先在 upstream 之间轮询，再在每个 upstream 的 API keys 之间轮询
- 自动处理 API key 轮换，最大化请求分发

### 🔥 热配置重载

- 配置文件修改后**自动热重载**，无需重启服务
- 使用 `notify` crate 实现跨平台文件监听
- 配置变更时平滑切换，不中断服务

### ⚡ 本地优化拦截

智能识别并本地处理特定请求，减少上游调用：

| 优化项 | 说明 |
|:-------|:------|
| 🔍 **Quota 检查拦截** | 对配额探测请求返回本地 mock 响应 |
| 📝 **快速前缀检测** | 识别并提取命令前缀（如 `git commit`） |
| 📋 **标题生成跳过** | 对标题生成请求返回默认响应 |
| 💡 **建议模式跳过** | 对建议模式请求返回空响应 |
| 📂 **文件路径提取** | 从命令输出中提取文件路径 |
| 📊 **历史分析跳过** | 对历史分析请求返回简化响应 |

### 📊 请求统计与监控

- 实时统计请求次数和 Token 消耗
- 区分用户输入 Token、历史上下文 Token、助手回复 Token
- 计算 Token 浪费比，帮助优化使用成本

---

## 🚀 快速开始

### 🎯 配置 Claude Code CLI

在你的 Claude Code CLI 配置中设置 API 端点：

```bash
# 方法 1: 环境变量
export ANTHROPIC_BASE_URL="http://127.0.0.1:9066/claude"
```

或者在 `~/.claude/settings.json` 中这样配置：

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "http://127.0.0.1:9066/claude",
    "ANTHROPIC_AUTH_TOKEN": "anything"
  }
}
```

其中：

- `ANTHROPIC_BASE_URL` 需要指向 `http://127.0.0.1:9066/claude`
- `ANTHROPIC_AUTH_TOKEN` 配置成什么都无所谓，本工具转发时会覆盖该值

### 📦 构建项目

```bash
# Debug 模式
sh build_native_stable.sh

# Release 模式（推荐，用于生产）
sh build_native_stable.sh r
```

### ⚙️ 配置

编辑 `config.toml`：

```toml
# upstream 可以配置多组，api_keys 支持多个 key
# 负载均衡策略：先选择 upstream，再轮询选择 key
# 修改配置后立即生效，无需重启

[[upstream]]
endpoint = "https://open.bigmodel.cn/api/anthropic"
model = "glm-4.7"
api_keys = ["your_api_key1", "your_api_key2"]

[[upstream]]
endpoint = "https://open.bigmodel.cn/api/anthropic"
model = "glm-4.7"
api_keys = ["your_api_key1", "your_api_key2"]

[optimizations]
enable_network_probe_mock = true           # 拦截配额探测请求
enable_fast_prefix_detection = true        # 快速前缀检测优化
enable_historical_analysis_mock = true     # 跳过历史分析请求
enable_title_generation_skip = true        # 跳过标题生成请求
enable_suggestion_mode_skip = true         # 跳过建议模式请求
enable_filepath_extraction_mock = true     # 文件路径提取优化
```

### ▶️ 测试运行

```bash
# 使用默认配置 (config.toml)
cargo r

# 指定配置文件
cargo r /path/to/config.toml
```

服务默认监听 `0.0.0.0:9066`。

---

## 📖 配置说明

### 🔌 upstream 配置

| 字段 | 类型 | 说明 |
|:-----|:------|:------|
| `endpoint` | `String` | 上游 API 地址 |
| `model` | `String` | 强制使用的模型名称 |
| `api_keys` | `Vec<String>` | API 密钥列表，支持多个 key 负载均衡 |

### ⚙️ optimizations 配置

| 字段 | 类型 | 默认值 | 说明 |
|:-----|:------|:-------|:------|
| `enable_network_probe_mock` | `bool` | `true` | 拦截配额探测请求 |
| `enable_fast_prefix_detection` | `bool` | `true` | 快速前缀检测优化 |
| `enable_historical_analysis_mock` | `bool` | `true` | 跳过历史分析请求 |
| `enable_title_generation_skip` | `bool` | `true` | 跳过标题生成请求 |
| `enable_suggestion_mode_skip` | `bool` | `true` | 跳过建议模式请求 |
| `enable_filepath_extraction_mock` | `bool` | `true` | 文件路径提取优化 |

---

## 🏗️ 工作原理

```
┌─────────────┐     ┌──────────────┐     ┌─────────────────┐
│   Client    │────▶│  CC Proxy    │────▶│  Upstream 1     │
│             │     │              │     │  (API Key 1)    │
│             │     │  🔄 负载均衡  │     │  (API Key 2)    │
│             │     │              ├────▶│  Upstream 2     │
│             │     │  ⚡ 本地优化  │     │  (API Key 1)    │
│             │     │  📊 Token统计 │     │  ...            │
└─────────────┘     └──────────────┘     └─────────────────┘
```

---

## 🛠️ 技术栈

| 技术 | 说明 |
|:-----|:------|
| **[Salvo](https://salvo.rs/)** | 高性能异步 Web 框架 |
| **[Hyper](https://hyper.rs/)** | 成熟的 HTTP/1.1 & HTTP/2 实现 |
| **[Tokio](https://tokio.rs/)** | Rust 异步运行时核心 |
| **[arc-swap](https://docs.rs/arc-swap/)** | 无锁配置热更新 |
| **[notify](https://docs.rs/notify/)** | 跨平台文件监听 |
| **[mimalloc](https://github.com/microsoft/mimalloc)** | 高性能内存分配器 |

---

## ⚡ 性能优化

- ✅ Release 构建使用 LTO (Link Time Optimization)
- ✅ 使用 mimalloc 替代默认分配器
- ✅ HTTP 连接复用，减少连接开销
- ✅ 无锁配置更新，避免锁竞争

---

## 📄 许可证

GNU General Public License v3.0 (GPLv3)
