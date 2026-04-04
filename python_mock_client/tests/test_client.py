import pytest
import sys
import os

# Add parent directory to path to import client
sys.path.append(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from client import MockLLMClient

class TestMockLLMClient:
    
    @pytest.fixture
    def client(self):
        return MockLLMClient(config={"latency_ms": 0})

    def test_openai_request_processing(self, client):
        """Test processing of a valid OpenAI ChatCompletion request."""
        url = "/v1/chat/completions"
        headers = {"Authorization": "Bearer sk-test"}
        body = {
            "model": "gpt-3.5-turbo",
            "messages": [{"role": "user", "content": "Hello!"}],
            "temperature": 0.7
        }

        response = client.process_request(url, headers, body)

        assert response["object"] == "chat.completion"
        assert response["model"] == "gpt-3.5-turbo"
        assert len(response["choices"]) == 1
        assert response["choices"][0]["message"]["role"] == "assistant"
        assert "mock response" in response["choices"][0]["message"]["content"]
        assert "usage" in response

    def test_anthropic_request_processing(self, client):
        """Test processing of a valid Anthropic Messages API request."""
        url = "/v1/messages"
        headers = {"x-api-key": "sk-ant-test", "anthropic-version": "2023-06-01"}
        body = {
            "model": "claude-3-opus-20240229",
            "messages": [{"role": "user", "content": "Hi there!"}],
            "max_tokens": 1024
        }

        response = client.process_request(url, headers, body)

        assert response["type"] == "message"
        assert response["model"] == "claude-3-opus-20240229"
        assert response["role"] == "assistant"
        assert len(response["content"]) == 1
        assert response["content"][0]["type"] == "text"
        assert "mock response" in response["content"][0]["text"]
        assert "usage" in response

    def test_openai_routing_by_model_name(self, client):
        """Test routing to OpenAI handler based on model name in body."""
        url = "/v1/unknown" # ambiguous URL
        headers = {}
        body = {
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Test"}],
        }
        
        # Should detect GPT and try OpenAI parsing
        response = client.process_request(url, headers, body)
        assert response["object"] == "chat.completion"

    def test_anthropic_routing_by_model_name(self, client):
        """Test routing to Anthropic handler based on model name in body."""
        url = "/v1/unknown"
        headers = {}
        body = {
            "model": "claude-3",
            "messages": [{"role": "user", "content": "Test"}],
            "max_tokens": 100
        }

        response = client.process_request(url, headers, body)
        assert response["type"] == "message"

    def test_invalid_openai_request(self, client):
        """Test that invalid OpenAI requests raise ValueError."""
        url = "/v1/chat/completions"
        headers = {}
        body = {
            "model": "gpt-3.5-turbo",
            # Missing messages
        }

        with pytest.raises(ValueError, match="Invalid OpenAI request format"):
            client.process_request(url, headers, body)

    def test_invalid_anthropic_request(self, client):
        """Test that invalid Anthropic requests raise ValueError."""
        url = "/v1/messages"
        headers = {}
        body = {
            "model": "claude-3",
            "messages": [],
            # Missing max_tokens
        }

        with pytest.raises(ValueError, match="Invalid Anthropic request format"):
            client.process_request(url, headers, body)

    def test_unknown_provider(self, client):
        """Test that unknown provider raises ValueError."""
        url = "/v1/unknown"
        headers = {}
        body = {"foo": "bar"}

        with pytest.raises(ValueError, match="Could not determine LLM provider"):
            client.process_request(url, headers, body)

    def test_custom_config(self):
        """Test that custom configuration is applied."""
        custom_text = "Custom Reply"
        client = MockLLMClient(config={"default_response_text": custom_text})
        
        url = "/v1/chat/completions"
        headers = {}
        body = {
            "model": "gpt-3.5-turbo",
            "messages": [{"role": "user", "content": "Hi"}],
        }

        response = client.process_request(url, headers, body)
        assert custom_text in response["choices"][0]["message"]["content"]
