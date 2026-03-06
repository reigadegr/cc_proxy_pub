//! SSE 流式格式转换
//!
//! 将 `OpenAI` Responses API 的 SSE 流实时转换为 Anthropic Claude API 的 SSE 流
//!
//! ## `OpenAI` Responses SSE 格式
//! ```text
//! data: {"type":"response.output.done","output":[...],"status":"completed"}
//! ```
//!
//! ## Anthropic Claude SSE 格式
//! ```text
//! event: message_start
//! data: {"type":"message_start","message":{...}}
//!
//! event: content_block_start
//! data: {"type":"content_block_start",...}
//!
//! event: content_block_delta
//! data: {"type":"content_block_delta",...}
//!
//! event: content_block_stop
//! data: {"type":"content_block_stop"}
//!
//! event: message_delta
//! data: {"type":"message_delta",...}
//!
//! event: message_stop
//! data: {"type":"message_stop"}
//! ```

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use futures_util::Stream;
use http_body_util::BodyStream;
use hyper::body::Incoming;
use serde_json::{Value, json};

/// `OpenAI` Responses 流式转换器状态
#[derive(Debug, Default)]
enum StreamingState {
    /// 尚未开始
    #[default]
    NotStarted,
    /// 已发送 `message_start，正在处理内容`
    InContent {
        /// 当前累积的文本内容
        text_buffer: String,
        /// 当前累积的 thinking 内容
        thinking_buffer: String,
        /// 工具调用列表
        tool_uses: Vec<Value>,
        /// 是否已发送 `content_block_start`
        block_started: bool,
        /// 当前块类型：text, thinking, 或 `tool_use`
        current_block_type: Option<String>,
    },
    /// 已完成
    Done,
}

/// `OpenAI` Responses SSE 流转换为 Anthropic SSE 流
pub struct ResponsesStreamConverter {
    /// 底层 HTTP body 流
    inner: BodyStream<Incoming>,
    /// 转换状态
    state: StreamingState,
    /// 模型名称（用于响应）
    model: Option<String>,
    /// 待发送的事件队列
    event_queue: Vec<Bytes>,
    /// 是否结束
    finished: bool,
    /// 响应 ID
    response_id: String,
}

impl ResponsesStreamConverter {
    /// 创建新的流转换器
    pub fn new(inner: BodyStream<Incoming>, model: Option<String>) -> Self {
        Self {
            inner,
            state: StreamingState::NotStarted,
            model,
            event_queue: Vec::new(),
            finished: false,
            response_id: "msg_proxy".to_string(),
        }
    }

    /// 处理单个 `OpenAI` Responses 数据块
    fn process_chunk(&mut self, chunk: &Bytes) {
        let Ok(data_str) = std::str::from_utf8(chunk) else {
            tracing::warn!("无法将 SSE 数据块解析为 UTF-8");
            return;
        };

        tracing::debug!("🔍 处理 SSE 块: {}", data_str);

        // SSE 格式：每行可能是 "data: {...}" 或空行
        for line in data_str.lines() {
            let line = line.trim();

            // 跳过空行和注释
            if line.is_empty() || line.starts_with(':') {
                continue;
            }

            // 解析 "data: {...}" 格式
            if let Some(json_str) = line.strip_prefix("data:") {
                let json_str = json_str.trim();

                // 检查是否为结束标记
                if json_str == "[DONE]" {
                    tracing::debug!("收到 [DONE] 标记");
                    self.finalize_stream();
                    continue;
                }

                // 解析 JSON
                match serde_json::from_str::<Value>(json_str) {
                    Ok(event) => self.process_openai_event(&event),
                    Err(e) => {
                        tracing::warn!("解析 OpenAI SSE 事件 JSON 失败: {}, 原始: {}", e, json_str);
                    }
                }
            }
        }
    }

    /// 处理单个 `OpenAI` Responses 事件
    fn process_openai_event(&mut self, event: &Value) {
        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

        tracing::debug!("📨 OpenAI 事件类型: {}", event_type);

        match event_type {
            "response.created" => {
                // 响应创建，提取 ID
                if let Some(id) = event.get("id").and_then(|v| v.as_str()) {
                    self.response_id = id.to_string();
                }
                self.enqueue_message_start();
            }
            "response.output.add.done" => {
                // 增量内容添加完成
                if let Some(output) = event.get("output").and_then(|v| v.as_array()) {
                    self.process_output_items(output);
                }
            }
            "response.output.done" => {
                // 输出完成
                if let Some(output) = event.get("output").and_then(|v| v.as_array()) {
                    self.process_output_items(output);
                }

                // 检查状态
                let status = event
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("completed");
                let has_tool_uses = if let StreamingState::InContent { tool_uses, .. } = &self.state
                {
                    !tool_uses.is_empty()
                } else {
                    false
                };

                self.finalize_with_status(status, has_tool_uses);
            }
            "response.done" => {
                // 响应完成
                if let Some(output) = event
                    .get("response")
                    .and_then(|v| v.get("output"))
                    .and_then(|v| v.as_array())
                {
                    self.process_output_items(output);
                }

                let status = event
                    .get("response")
                    .and_then(|v| v.get("status"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("completed");
                let has_tool_uses = if let StreamingState::InContent { tool_uses, .. } = &self.state
                {
                    !tool_uses.is_empty()
                } else {
                    false
                };

                self.finalize_with_status(status, has_tool_uses);
            }
            "error" => {
                tracing::error!("OpenAI 错误事件: {}", event);
                self.enqueue_error("Upstream stream error");
            }
            _ => {
                tracing::debug!("未处理的事件类型: {}", event_type);
            }
        }
    }

    /// 处理 output 数组中的项目
    fn process_output_items(&mut self, output: &[Value]) {
        for item in output {
            let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match item_type {
                "message" => {
                    if let Some(content) = item.get("content").and_then(|v| v.as_array()) {
                        for part in content {
                            let part_type = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            match part_type {
                                "output_text" => {
                                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                        self.add_text(text);
                                    }
                                }
                                "reasoning_text" => {
                                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                        self.add_thinking(text);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                "function_call" => {
                    self.add_tool_use(item);
                }
                _ => {}
            }
        }
    }

    /// 发送 `message_start` 事件
    fn enqueue_message_start(&mut self) {
        if matches!(self.state, StreamingState::NotStarted) {
            let model = self.model.as_deref().unwrap_or("unknown");

            let event = json!({
                "type": "message_start",
                "message": {
                    "id": self.response_id,
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": model,
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {
                        "input_tokens": 0,
                        "output_tokens": 0
                    }
                }
            });

            self.enqueue_sse_event("message_start", &event);

            self.state = StreamingState::InContent {
                text_buffer: String::new(),
                thinking_buffer: String::new(),
                tool_uses: Vec::new(),
                block_started: false,
                current_block_type: None,
            };
        }
    }

    /// 添加文本内容
    fn add_text(&mut self, text: &str) {
        // 先检查当前状态，提取需要的信息
        let (need_new_block, existing_block_started) = {
            let StreamingState::InContent {
                block_started,
                current_block_type,
                ..
            } = &self.state
            else {
                return;
            };

            let need_new = current_block_type.as_deref() != Some("text");
            let started = *block_started;
            (need_new, started)
        };

        // 借用结束后再执行操作
        if need_new_block {
            if existing_block_started {
                self.enqueue_content_block_stop();
            }
            self.enqueue_content_block_start("text");
            // 更新状态
            if let StreamingState::InContent {
                ref mut block_started,
                ref mut current_block_type,
                ..
            } = self.state
            {
                *current_block_type = Some("text".to_string());
                *block_started = true;
            }
        }

        // 添加文本
        if let StreamingState::InContent {
            ref mut text_buffer,
            ..
        } = self.state
        {
            text_buffer.push_str(text);
        }

        // 发送 delta
        let delta = json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": text}
        });
        self.enqueue_sse_event("content_block_delta", &delta);
    }

    /// 添加 thinking 内容
    fn add_thinking(&mut self, text: &str) {
        // 先检查当前状态，提取需要的信息
        let (need_new_block, existing_block_started) = {
            let StreamingState::InContent {
                block_started,
                current_block_type,
                ..
            } = &self.state
            else {
                return;
            };

            let need_new = current_block_type.as_deref() != Some("thinking");
            let started = *block_started;
            (need_new, started)
        };

        // 借用结束后再执行操作
        if need_new_block {
            if existing_block_started {
                self.enqueue_content_block_stop();
            }
            self.enqueue_content_block_start("thinking");
            // 更新状态
            if let StreamingState::InContent {
                ref mut block_started,
                ref mut current_block_type,
                ..
            } = self.state
            {
                *current_block_type = Some("thinking".to_string());
                *block_started = true;
            }
        }

        // 添加 thinking 内容
        if let StreamingState::InContent {
            ref mut thinking_buffer,
            ..
        } = self.state
        {
            thinking_buffer.push_str(text);
        }

        // 发送 delta
        let delta = json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "thinking_delta", "thinking": text}
        });
        self.enqueue_sse_event("content_block_delta", &delta);
    }

    /// 添加工具调用
    fn add_tool_use(&mut self, item: &Value) {
        // 先检查当前状态
        let (need_new_block, existing_block_started) = {
            let StreamingState::InContent {
                block_started,
                current_block_type,
                ..
            } = &self.state
            else {
                return;
            };

            let need_new = current_block_type.as_deref() != Some("tool_use");
            let started = *block_started;
            (need_new, started)
        };

        // 借用结束后再执行操作
        if need_new_block {
            if existing_block_started {
                self.enqueue_content_block_stop();
            }
            // 更新状态
            if let StreamingState::InContent {
                ref mut current_block_type,
                ref mut block_started,
                ..
            } = self.state
            {
                *current_block_type = Some("tool_use".to_string());
                *block_started = true;
            }
        }

        // 解析工具调用
        let call_id = item
            .get("call_id")
            .and_then(|v| v.as_str())
            .or_else(|| item.get("id").and_then(|v| v.as_str()))
            .unwrap_or("");

        let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let arguments_str = item
            .get("arguments")
            .and_then(|v| v.as_str())
            .unwrap_or("{}");

        let input = serde_json::from_str::<Value>(arguments_str)
            .unwrap_or_else(|_| json!({"_raw": arguments_str}));

        let tool_use = json!({
            "type": "tool_use",
            "id": call_id,
            "name": name,
            "input": input
        });

        // 获取索引并添加到 tool_uses
        let index = {
            let StreamingState::InContent { tool_uses, .. } = &mut self.state else {
                return;
            };
            let idx = tool_uses.len();
            tool_uses.push(tool_use.clone());
            idx
        };

        // 发送 content_block_start
        let start_event = json!({
            "type": "content_block_start",
            "index": index,
            "content_block": tool_use
        });
        self.enqueue_sse_event("content_block_start", &start_event);

        // 发送 content_block_delta（空）
        let delta_event = json!({
            "type": "content_block_delta",
            "index": index,
            "delta": {"type": "empty"}
        });
        self.enqueue_sse_event("content_block_delta", &delta_event);

        // 发送 content_block_stop
        self.enqueue_sse_event("content_block_stop", &json!({"type": "content_block_stop"}));
    }

    /// 发送 `content_block_start`
    fn enqueue_content_block_start(&mut self, block_type: &str) {
        let content_block = match block_type {
            "thinking" => json!({
                "type": "thinking",
                "thinking": ""
            }),
            _ => json!({
                "type": "text",
                "text": ""
            }),
        };

        let event = json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": content_block
        });
        self.enqueue_sse_event("content_block_start", &event);
    }

    /// 发送 `content_block_stop`
    fn enqueue_content_block_stop(&mut self) {
        self.enqueue_sse_event("content_block_stop", &json!({"type": "content_block_stop"}));
    }

    /// 完成流式响应
    fn finalize_with_status(&mut self, status: &str, has_tool_uses: bool) {
        if !self.finished {
            // 确定停止原因
            let stop_reason = match status {
                "incomplete" => "max_tokens",
                "completed" if has_tool_uses => "tool_use",
                _ => "end_turn",
            };

            self.finalize_stream_with_reason(stop_reason);
        }
    }

    /// 完成流式响应（带停止原因）
    fn finalize_stream_with_reason(&mut self, stop_reason: &str) {
        if self.finished {
            return;
        }

        // 如果当前在内容块中，先结束它
        if matches!(
            self.state,
            StreamingState::InContent {
                block_started: true,
                ..
            }
        ) {
            self.enqueue_content_block_stop();
        }

        // 发送 message_delta
        let delta_event = json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason, "stop_sequence": null},
            "usage": {"output_tokens": 0}
        });
        self.enqueue_sse_event("message_delta", &delta_event);

        // 发送 message_stop
        self.enqueue_sse_event("message_stop", &json!({"type": "message_stop"}));

        self.finished = true;
        self.state = StreamingState::Done;

        tracing::debug!("✅ 流式转换完成: stop_reason={}", stop_reason);
    }

    /// 完成流式响应（默认）
    fn finalize_stream(&mut self) {
        self.finalize_stream_with_reason("end_turn");
    }

    /// 发送错误事件
    fn enqueue_error(&mut self, message: &str) {
        let error_event = json!({
            "type": "error",
            "error": {
                "type": "internal_error",
                "message": message
            }
        });
        self.enqueue_sse_event("error", &error_event);
        self.finished = true;
    }

    /// 将事件加入队列
    fn enqueue_sse_event(&mut self, event_type: &str, data: &Value) {
        let data_str = serde_json::to_string(data).unwrap_or_else(|_| "{}".to_string());

        // SSE 格式：event: xxx\ndata: xxx\n\n
        let mut sse_string = String::new();
        sse_string.push_str("event: ");
        sse_string.push_str(event_type);
        sse_string.push('\n');
        sse_string.push_str("data: ");
        sse_string.push_str(&data_str);
        sse_string.push_str("\n\n");

        self.event_queue.push(Bytes::from(sse_string));
    }
}

impl Stream for ResponsesStreamConverter {
    type Item = Result<Bytes, std::convert::Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // 首先检查队列中是否有待发送的事件
        if !self.event_queue.is_empty() {
            let event = self.event_queue.remove(0);
            return Poll::Ready(Some(Ok(event)));
        }

        // 如果已完成，返回 None
        if self.finished {
            return Poll::Ready(None);
        }

        // 从底层流读取更多数据
        match Pin::new(&mut self.inner).poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                if let Some(data) = chunk.data_ref() {
                    self.process_chunk(data);
                }

                // 处理后检查队列
                if !self.event_queue.is_empty() {
                    let event = self.event_queue.remove(0);
                    Poll::Ready(Some(Ok(event)))
                } else if self.finished {
                    Poll::Ready(None)
                } else {
                    // 继续等待更多数据
                    Poll::Pending
                }
            }
            Poll::Ready(Some(Err(e))) => {
                tracing::error!("流读取错误: {}", e);
                self.enqueue_error(&format!("Stream error: {e}"));
                let event = self.event_queue.remove(0);
                Poll::Ready(Some(Ok(event)))
            }
            Poll::Ready(None) => {
                // 上游流结束
                if !self.finished {
                    self.finalize_stream();
                }
                if self.event_queue.is_empty() {
                    Poll::Ready(None)
                } else {
                    let event = self.event_queue.remove(0);
                    Poll::Ready(Some(Ok(event)))
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }
}
