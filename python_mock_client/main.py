import json
from client import MockLLMClient

def main():
    # Initialize the client with custom configuration
    client = MockLLMClient(config={
        "default_response_text": "Greetings from the Mock LLM!",
        "latency_ms": 100  # Simulate 100ms latency
    })

    print("=== OpenAI Request Example ===")
    openai_url = "/v1/chat/completions"
    openai_headers = {
        "Authorization": "Bearer sk-mock-key",
        "Content-Type": "application/json"
    }
    openai_body = {
        "model": "gpt-4",
        "messages": [
            {"role": "system", "content": "You are a helpful assistant."},
            {"role": "user", "content": "Tell me a joke."}
        ],
        "temperature": 0.7
    }

    try:
        response = client.process_request(openai_url, openai_headers, openai_body)
        print(f"Status: 200 OK")
        print(json.dumps(response, indent=2))
    except Exception as e:
        print(f"Error: {e}")

    print("\n=== Anthropic Request Example ===")
    anthropic_url = "/v1/messages"
    anthropic_headers = {
        "x-api-key": "sk-ant-mock-key",
        "anthropic-version": "2023-06-01",
        "Content-Type": "application/json"
    }
    anthropic_body = {
        "model": "claude-3-sonnet-20240229",
        "max_tokens": 1024,
        "messages": [
            {"role": "user", "content": "Explain quantum physics briefly."}
        ]
    }

    try:
        response = client.process_request(anthropic_url, anthropic_headers, anthropic_body)
        print(f"Status: 200 OK")
        print(json.dumps(response, indent=2))
    except Exception as e:
        print(f"Error: {e}")

if __name__ == "__main__":
    main()
