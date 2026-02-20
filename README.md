# CC Proxy

一个高性能的 AI API 代理网关，支持多上游负载均衡、本地优化拦截和热配置重载。

## 功能特性

### 多上游负载均衡
- 支持配置多个 upstream 服务提供商
- **双层轮询策略**：先在 upstream 之间轮询，再在每个 upstream 的 API keys 之间轮询
- 自动处理 API key 轮换，最大化请求分发

### 热配置重载
- 配置文件修改后**自动热重载**，无需重启服务
- 使用 `notify` crate 实现跨平台文件监听
- 配置变更时平滑切换，不中断服务

### 本地优化拦截
智能识别并本地处理特定请求，减少上游调用：

| 优化项 | 说明 |
|--------|------|
| Quota 检查拦截 | 对配额探测请求返回本地 mock 响应 |
| 快速前缀检测 | 识别并提取命令前缀（如 `git commit`） |
| 标题生成跳过 | 对标题生成请求返回默认响应 |
| 建议模式跳过 | 对建议模式请求返回空响应 |
| 文件路径提取 | 从命令输出中提取文件路径 |

### 请求统计与监控
- 实时统计请求次数和 Token 消耗
- 区分用户输入 Token、历史上下文 Token、助手回复 Token
- 计算 Token 浪费比，帮助优化使用成本

## 快速开始

### 构建项目

```bash
# Debug 模式
sh build_native_stable.sh

# Release 模式（推荐，用于生产）
sh build_native_stable.sh r
```

### 配置

编辑 `config.toml`：

```toml
[[upstream]]
endpoint = "https://open.bigmodel.cn/api/anthropic"
model = "glm-4.7"
api_keys = ["your_api_key1", "your_api_key2"]

[[upstream]]
endpoint = "https://open.bigmodel.cn/api/anthropic"
model = "glm-5"
api_keys = ["your_api_key1", "your_api_key2"]

[optimizations]
enable_network_probe_mock = true
enable_fast_prefix_detection = true
enable_title_generation_skip = true
enable_suggestion_mode_skip = true
enable_filepath_extraction_mock = true

```

### 运行

```bash
# 使用默认配置 (config.toml)
cargo r

# 指定配置文件
cargo r /path/to/config.toml
```

服务默认监听 `0.0.0.0:9066`。

## 配置说明

### upstream

| 字段 | 类型 | 说明 |
|------|------|------|
| `endpoint` | String | 上游 API 地址 |
| `model` | String | 强制使用的模型名称 |
| `api_keys` | Vec<String> | API 密钥列表，支持多个 key 负载均衡 |

### optimizations

| 字段 | 类型 | 默认 | 说明 |
|------|------|------|------|
| `enable_network_probe_mock` | bool | true | 拦截配额探测请求 |
| `enable_fast_prefix_detection` | bool | true | 快速前缀检测优化 |
| `enable_title_generation_skip` | bool | true | 跳过标题生成请求 |
| `enable_suggestion_mode_skip` | bool | true | 跳过建议模式请求 |
| `enable_filepath_extraction_mock` | bool | true | 文件路径提取优化 |

## 工作原理

```
┌─────────────┐     ┌──────────────┐     ┌─────────────────┐
│   Client    │────▶│  CC Proxy    │────▶│  Upstream 1     │
│             │     │              │     │  (API Key 1)    │
│             │     │  负载均衡    │     │  (API Key 2)    │
│             │     │              ├────▶│  Upstream 2     │
│             │     │  本地优化    │     │  (API Key 1)    │
│             │     │  Token统计   │     │  ...            │
└─────────────┘     └──────────────┘     └─────────────────┘
```

## 技术栈

- **Web 框架**: [Salvo](https://salvo.rs/) - 高性能异步 HTTP 框架
- **HTTP 客户端**: [Hyper](https://hyper.rs/) - 成熟的 HTTP/1.1 & HTTP/2 实现
- **异步运行时**: [Tokio](https://tokio.rs/) - Rust 异步生态核心
- **配置管理**: [arc-swap](https://docs.rs/arc-swap/) - 无锁配置热更新
- **文件监控**: [notify](https://docs.rs/notify/) - 跨平台文件监听
- **内存分配**: [mimalloc](https://github.com/microsoft/mimalloc) - 高性能内存分配器

## 性能优化

- Release 构建使用 LTO (Link Time Optimization)
- 使用 mimalloc 替代默认分配器
- HTTP 连接复用，减少连接开销
- 无锁配置更新，避免锁竞争

## 许可证

MIT License
