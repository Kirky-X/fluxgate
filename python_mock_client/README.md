# Mock LLM Client

A Python library that simulates OpenAI and Anthropic LLM API behaviors. This client is designed to help test the SmartLlmProxy system and other LLM-integrated applications without incurring actual API costs.

## Features

- **Unified Interface**: Single `MockLLMClient` class to handle both OpenAI and Anthropic requests.
- **Automatic Routing**: Detects provider based on URL path, model name, or headers.
- **Protocol Compliance**:
  - Implements OpenAI `ChatCompletion` request/response format.
  - Implements Anthropic `Messages API` request/response format.
- **Configurable**: Support for custom latency and default response text.
- **Type Safe**: Uses `Pydantic` models for request validation and response construction.

## Requirements

- Python 3.8+
- pydantic
- pytest (for testing)

## Installation

```bash
pip install -r requirements.txt
```

## Running as a Server

To use this mock as a backend service (e.g., for integration testing with the Proxy):

1. Install server dependencies:
   ```bash
   pip install fastapi uvicorn
   ```

2. Start the server:
   ```bash
   python server.py
   ```
   The server listens on port `8001` by default.

3. Send requests:

   **OpenAI Format:**
   ```bash
   curl -X POST http://localhost:8001/v1/chat/completions \
     -H "Content-Type: application/json" \
     -d '{
       "model": "gpt-4",
       "messages": [{"role": "user", "content": "Hello"}]
     }'
   ```

   **Anthropic Format:**
   ```bash
   curl -X POST http://localhost:8001/v1/messages \
     -H "Content-Type: application/json" \
     -d '{
       "model": "claude-3-opus-20240229",
       "messages": [{"role": "user", "content": "Hello"}],
       "max_tokens": 1024
     }'
   ```

## Usage

See `main.py` for a complete runnable example.

```python
from client import MockLLMClient

# Initialize
client = MockLLMClient(config={"latency_ms": 50})

# Process OpenAI Request
response = client.process_request(
    url="/v1/chat/completions",
    headers={"Authorization": "Bearer ..."},
    body={
        "model": "gpt-4",
        "messages": [{"role": "user", "content": "Hello"}]
    }
)
print(response["choices"][0]["message"]["content"])
```

## Testing

Run the test suite with pytest:

```bash
pytest
```

## Design Compliance

This implementation follows the specifications in `SmartLlmProxy/prd.md`:
- **Section 5.1 (LLM Interface Design)**: Supports `/v1/chat/completions` and `/v1/messages` endpoints.
- **Section 4.1 (Unified Data Model)**: While this client mocks the *external* APIs, it ensures the data structures (like `usage` stats, `choices`, `content`) match what the proxy expects to receive from real vendors.
