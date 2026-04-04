# Python Mock Client

**Parent:** `../AGENTS.md`

## OVERVIEW

Python 模拟客户端，用于测试 LLM 代理系统的 mock 服务器和端到端测试。

## STRUCTURE

```
python_mock_client/
├── client.py              # 核心客户端实现
├── end_to_end_test.py     # 端到端测试
├── main.py                # 入口脚本
├── models.py              # 数据模型 (Pydantic)
├── server.py              # Mock 服务器
├── tests/                 # Python 测试用例
└── requirements.txt       # Python 依赖
```

## WHERE TO LOOK

| Task | File | Notes |
|------|------|-------|
| 客户端实现 | `client.py` | LLM API 调用封装 |
| Mock 服务器 | `server.py` | 模拟 OpenAI/Anthropic API |
| E2E 测试 | `end_to_end_test.py` | 完整流程测试 |
| 数据模型 | `models.py` | Pydantic 模型定义 |

## CONVENTIONS

- **依赖**: Pydantic ≥2.0.0, pytest ≥7.0.0
- **运行**: `python main.py` 启动服务器
- **测试**: `pytest tests/` 运行 Python 测试
- **服务器模式**: FastAPI + uvicorn 支持

## COMMANDS

```bash
pip install -r requirements.txt  # 安装依赖
python main.py                   # 启动 mock 服务器
pytest tests/                    # 运行 Python 测试
```
