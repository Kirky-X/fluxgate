from fastapi import FastAPI, Request, Response
from fastapi.responses import StreamingResponse
from client import MockLLMClient
import uvicorn
import os
import types
import collections.abc

app = FastAPI(title="Mock LLM Backend")

# Initialize Client
# You can override config via environment variables if needed
client = MockLLMClient(config={
    "default_response_text": "This is a response from the Mock Backend.",
    "latency_min_ms": 100,  # 100ms
    "latency_max_ms": 500,  # 500ms
})

@app.api_route("/{full_path:path}", methods=["GET", "POST", "PUT", "DELETE"])
async def catch_all(full_path: str, request: Request):
    """
    Catch-all route to forward all requests to the MockLLMClient.
    """
    # 1. Extract necessary info
    # Prepend "/" because FastAPI strips it, but our logic might expect "/v1/..."
    url = f"/{full_path}" 
    
    headers = dict(request.headers)
    
    try:
        body = await request.json()
    except Exception:
        body = {}

    print(f"Received request: {request.method} {url}")
    # print(f"Headers: {headers}")
    # print(f"Body: {body}")

    try:
        # 2. Process Request
        result = client.process_request(url, headers, body)
        
        # 3. Check if result is a generator (Streaming)
        if isinstance(result, types.GeneratorType) or isinstance(result, collections.abc.Iterator):
            return StreamingResponse(result, media_type="text/event-stream")
        
        # 4. Normal Response
        return result
        
    except ValueError as e:
        # Return 400 for invalid requests (routing failure or validation failure)
        return Response(content=str(e), status_code=400)
    except Exception as e:
        return Response(content=f"Internal Error: {str(e)}", status_code=500)

if __name__ == "__main__":
    port = int(os.getenv("PORT", 8001))
    uvicorn.run(app, host="0.0.0.0", port=port)
