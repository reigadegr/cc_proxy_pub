curl -X POST http://localhost:9066/api/paas/v4/chat/completions \
  -H "content-type: application/json" \
  -d '{"model": "glm-4.7-flash", "messages": [{"role": "user", "content": "你好"}]}'
