import json
import uuid
import time
import random
from typing import Dict, Any, Union, Optional, Iterator
from pydantic import ValidationError

from models import (
    OpenAIRequest, OpenAIResponse, OpenAIMessage, OpenAIChoice, OpenAIUsage,
    AnthropicRequest, AnthropicResponse, AnthropicMessage, AnthropicContent, AnthropicUsage
)

class MockLLMClient:
    """
    A unified Mock LLM Client that supports both OpenAI and Anthropic request types.
    It routes requests based on URL patterns, headers, or request body content.
    """

    def __init__(self, config: Optional[Dict[str, Any]] = None):
        """
        Initialize the Mock Client with optional configuration.
        
        Args:
            config: Dictionary containing configuration options like:
                    - default_response_text: str
                    - latency_min_ms: int (Minimum latency in ms)
                    - latency_max_ms: int (Maximum latency in ms)
                    - latency_ms: int (Fixed latency, overrides min/max if present)
                    - error_rate: float (0.0 to 1.0)
        """
        self.config = config or {}
        self.default_response_text = self.config.get("default_response_text", "This is a mock response from the LLM client.")
        
        # Latency configuration
        if "latency_ms" in self.config:
            self.latency_min_ms = self.latency_max_ms = self.config["latency_ms"]
        else:
            self.latency_min_ms = self.config.get("latency_min_ms", 0)
            self.latency_max_ms = self.config.get("latency_max_ms", 0)

    def _sleep_random_latency(self):
        """Sleep for a random duration based on configuration."""
        if self.latency_max_ms > 0:
            delay = random.uniform(self.latency_min_ms, self.latency_max_ms) / 1000.0
            time.sleep(delay)

    def process_request(self, url: str, headers: Dict[str, str], body: Dict[str, Any]) -> Union[Dict[str, Any], Iterator[str]]:
        """
        Main entry point to process an LLM request.
        
        Args:
            url: The request URL path (e.g., "/v1/chat/completions").
            headers: Request headers.
            body: Request body as a dictionary.
            
        Returns:
            A dictionary representing the JSON response OR an iterator of strings (SSE events) if streaming.
            
        Raises:
            ValueError: If the request format is unrecognized or invalid.
        """
        # Determine Provider
        is_openai = "/chat/completions" in url or "gpt" in body.get("model", "").lower()
        is_anthropic = "/messages" in url or "claude" in body.get("model", "").lower()

        # Fallback using headers
        if not (is_openai or is_anthropic):
            if "anthropic-version" in headers:
                is_anthropic = True
            else:
                raise ValueError("Could not determine LLM provider from request.")

        # Handle Request
        if is_openai:
            return self._handle_openai(body)
        elif is_anthropic:
            return self._handle_anthropic(body)
        else:
            # Should not reach here
            raise ValueError("Could not determine LLM provider from request.")

    def _handle_openai(self, body: Dict[str, Any]) -> Union[Dict[str, Any], Iterator[str]]:
        """
        Handle OpenAI-formatted requests.
        """
        try:
            req = OpenAIRequest(**body)
        except ValidationError as e:
            raise ValueError(f"Invalid OpenAI request format: {e}")

        # Handle content being list or str
        last_msg_content = req.messages[-1].content
        if isinstance(last_msg_content, list):
            # Extract text from list if possible
            text_parts = [p.get("text", "") for p in last_msg_content if p.get("type") == "text"]
            input_text = " ".join(text_parts)
        else:
            input_text = str(last_msg_content)

        response_text = self._generate_text(input_text)
        
        if req.stream:
            return self._generate_openai_stream(req.model, response_text)

        # Non-streaming response
        self._sleep_random_latency()
        
        prompt_tokens = len(str(req.messages)) // 4
        completion_tokens = len(response_text) // 4
        total_tokens = prompt_tokens + completion_tokens

        response = OpenAIResponse(
            id=f"chatcmpl-{uuid.uuid4()}",
            model=req.model,
            choices=[
                OpenAIChoice(
                    index=0,
                    message=OpenAIMessage(role="assistant", content=response_text),
                    finish_reason="stop"
                )
            ],
            usage=OpenAIUsage(
                prompt_tokens=prompt_tokens,
                completion_tokens=completion_tokens,
                total_tokens=total_tokens
            )
        )
        return response.model_dump()

    def _handle_anthropic(self, body: Dict[str, Any]) -> Union[Dict[str, Any], Iterator[str]]:
        """
        Handle Anthropic-formatted requests.
        """
        try:
            req = AnthropicRequest(**body)
        except ValidationError as e:
            raise ValueError(f"Invalid Anthropic request format: {e}")

        response_text = self._generate_text(req.messages[-1].content)
        
        if req.stream:
            return self._generate_anthropic_stream(req.model, response_text)

        # Non-streaming response
        self._sleep_random_latency()

        input_tokens = len(str(req.messages)) // 4
        output_tokens = len(response_text) // 4

        response = AnthropicResponse(
            id=f"msg_{uuid.uuid4()}",
            model=req.model,
            content=[AnthropicContent(type="text", text=response_text)],
            usage=AnthropicUsage(
                input_tokens=input_tokens,
                output_tokens=output_tokens
            )
        )
        return response.model_dump()

    def _generate_text(self, input_text: str) -> str:
        return f"{self.default_response_text} (Context: Received '{input_text[:20]}...')"

    def _generate_openai_stream(self, model: str, text: str) -> Iterator[str]:
        """Generate OpenAI SSE stream events."""
        request_id = f"chatcmpl-{uuid.uuid4()}"
        created = int(time.time())
        
        # Chunk the text
        chunks = text.split(" ")
        
        # Initial chunk (role)
        msg_role = {
            "id": request_id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": None}]
        }
        self._sleep_random_latency()
        yield f"data: {json.dumps(msg_role)}\n\n"

        # Content chunks
        for chunk in chunks:
            self._sleep_random_latency()
            msg_content = {
                "id": request_id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model,
                "choices": [{"index": 0, "delta": {"content": chunk + " "}, "finish_reason": None}]
            }
            yield f"data: {json.dumps(msg_content)}\n\n"

        # Finish chunk
        self._sleep_random_latency()
        msg_finish = {
            "id": request_id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
        }
        yield f"data: {json.dumps(msg_finish)}\n\n"
        yield "data: [DONE]\n\n"

    def _generate_anthropic_stream(self, model: str, text: str) -> Iterator[str]:
        """Generate Anthropic SSE stream events."""
        msg_id = f"msg_{uuid.uuid4()}"
        
        # 1. message_start
        self._sleep_random_latency()
        yield f"event: message_start\ndata: {json.dumps({'type': 'message_start', 'message': {'id': msg_id, 'type': 'message', 'role': 'assistant', 'content': [], 'model': model, 'stop_reason': None, 'stop_sequence': None, 'usage': {'input_tokens': 10, 'output_tokens': 1}}})}\n\n"
        
        # 2. content_block_start
        self._sleep_random_latency()
        yield f"event: content_block_start\ndata: {json.dumps({'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'text', 'text': ''}})}\n\n"

        # 3. content_block_delta (stream text)
        chunks = text.split(" ")
        for chunk in chunks:
            self._sleep_random_latency()
            yield f"event: content_block_delta\ndata: {json.dumps({'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'text_delta', 'text': chunk + ' '}})}\n\n"

        # 4. content_block_stop
        self._sleep_random_latency()
        yield f"event: content_block_stop\ndata: {json.dumps({'type': 'content_block_stop', 'index': 0})}\n\n"

        # 5. message_delta
        self._sleep_random_latency()
        yield f"event: message_delta\ndata: {json.dumps({'type': 'message_delta', 'delta': {'stop_reason': 'end_turn', 'stop_sequence': None}, 'usage': {'output_tokens': len(text)//4}})}\n\n"

        # 6. message_stop
        self._sleep_random_latency()
        yield f"event: message_stop\ndata: {json.dumps({'type': 'message_stop'})}\n\n"
