from typing import List, Optional, Union, Literal, Dict, Any
from pydantic import BaseModel, Field
import time

# --- OpenAI Models ---

class OpenAIMessage(BaseModel):
    role: str
    content: Union[str, List[Dict[str, Any]]]

class OpenAIRequest(BaseModel):
    model: str
    messages: List[OpenAIMessage]
    temperature: Optional[float] = 1.0
    max_tokens: Optional[int] = None
    stream: Optional[bool] = False

class OpenAIChoice(BaseModel):
    index: int
    message: OpenAIMessage
    finish_reason: str = "stop"

class OpenAIUsage(BaseModel):
    prompt_tokens: int
    completion_tokens: int
    total_tokens: int

class OpenAIResponse(BaseModel):
    id: str
    object: str = "chat.completion"
    created: int = Field(default_factory=lambda: int(time.time()))
    model: str
    choices: List[OpenAIChoice]
    usage: OpenAIUsage

# --- Anthropic Models ---

class AnthropicMessage(BaseModel):
    role: str
    content: str

class AnthropicRequest(BaseModel):
    model: str
    messages: List[AnthropicMessage]
    max_tokens: int
    temperature: Optional[float] = 1.0
    stream: Optional[bool] = False

class AnthropicContent(BaseModel):
    type: str = "text"
    text: str

class AnthropicUsage(BaseModel):
    input_tokens: int
    output_tokens: int

class AnthropicResponse(BaseModel):
    id: str
    type: str = "message"
    role: str = "assistant"
    content: List[AnthropicContent]
    model: str
    stop_reason: str = "end_turn"
    stop_sequence: Optional[str] = None
    usage: AnthropicUsage
