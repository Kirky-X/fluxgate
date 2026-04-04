import unittest
import requests
import json
import time
import threading
import sys
import os

# Configuration
PROXY_URL = "http://127.0.0.1:8080"
OPENAI_ENDPOINT = f"{PROXY_URL}/v1/chat/completions"
ANTHROPIC_ENDPOINT = f"{PROXY_URL}/v1/messages"
ADMIN_ENDPOINT = f"{PROXY_URL}/admin"

class TestSmartLlmProxy(unittest.TestCase):
    def setUp(self):
        # Ensure the proxy is up (simple check)
        try:
            requests.get(f"{PROXY_URL}/health", timeout=1)
        except requests.exceptions.RequestException:
            # print("Proxy not reachable, waiting...")
            pass

    def test_01_health_check(self):
        """Test Health Check API"""
        response = requests.get(f"{PROXY_URL}/health")
        self.assertEqual(response.status_code, 200)
        data = response.json()
        self.assertEqual(data.get("status"), "ok")

    def test_02_openai_chat_completion(self):
        """Test OpenAI Chat Completion"""
        payload = {
            "model": "gpt-3.5-turbo",
            "messages": [{"role": "user", "content": "Hello OpenAI"}],
            "temperature": 0.7
        }
        headers = {"Authorization": "Bearer test-token"}
        
        response = requests.post(OPENAI_ENDPOINT, json=payload, headers=headers)
        if response.status_code != 200:
            print(f"\n[OpenAI] Error: {response.text}")
        self.assertEqual(response.status_code, 200)
        data = response.json()
        
        self.assertIn("choices", data)
        self.assertTrue(len(data["choices"]) > 0)
        self.assertEqual(data["model"], "gpt-3.5-turbo")
        print(f"\n[OpenAI] Response: {data['choices'][0]['message']['content']}")

    def test_03_anthropic_messages(self):
        """Test Anthropic Messages API"""
        payload = {
            "model": "claude-2",
            "messages": [{"role": "user", "content": "Hello Claude"}],
            "max_tokens": 100
        }
        headers = {
            "x-api-key": "test-key",
            "anthropic-version": "2023-06-01"
        }
        
        response = requests.post(ANTHROPIC_ENDPOINT, json=payload, headers=headers)
        if response.status_code != 200:
            print(f"\n[Anthropic] Error: {response.text}")
        self.assertEqual(response.status_code, 200)
        data = response.json()
        
        self.assertIn("content", data)
        self.assertTrue(len(data["content"]) > 0)
        self.assertEqual(data["model"], "claude-2")
        print(f"\n[Anthropic] Response: {data['content'][0]['text']}")

    def test_04_openai_stream(self):
        """Test OpenAI Streaming"""
        payload = {
            "model": "gpt-3.5-turbo",
            "messages": [{"role": "user", "content": "Stream me"}],
            "stream": True
        }
        headers = {"Authorization": "Bearer test-token"}
        
        response = requests.post(OPENAI_ENDPOINT, json=payload, headers=headers, stream=True)
        if response.status_code != 200:
            print(f"\n[OpenAI Stream] Error: {response.text}")
        self.assertEqual(response.status_code, 200)
        
        chunks_received = 0
        for line in response.iter_lines():
            if line:
                decoded_line = line.decode('utf-8')
                if decoded_line.startswith("data: "):
                    data_str = decoded_line[6:]
                    if data_str == "[DONE]":
                        break
                    try:
                        data = json.loads(data_str)
                        if "choices" in data and len(data["choices"]) > 0:
                            delta = data["choices"][0].get("delta", {})
                            if "content" in delta:
                                chunks_received += 1
                    except json.JSONDecodeError:
                        pass
        
        self.assertTrue(chunks_received > 0)
        print(f"\n[OpenAI Stream] Received {chunks_received} content chunks")

    def test_05_anthropic_stream(self):
        """Test Anthropic Streaming"""
        payload = {
            "model": "claude-2",
            "messages": [{"role": "user", "content": "Stream me"}],
            "max_tokens": 100,
            "stream": True
        }
        headers = {
            "x-api-key": "test-key",
            "anthropic-version": "2023-06-01"
        }
        
        response = requests.post(ANTHROPIC_ENDPOINT, json=payload, headers=headers, stream=True)
        self.assertEqual(response.status_code, 200)
        
        events_received = 0
        for line in response.iter_lines():
            if line:
                decoded_line = line.decode('utf-8')
                if decoded_line.startswith("event: "):
                    event_type = decoded_line[7:]
                    if event_type == "content_block_delta":
                        events_received += 1
        
        self.assertTrue(events_received > 0)
        print(f"\n[Anthropic Stream] Received {events_received} content block deltas")

    def test_06_admin_config(self):
        """Test Admin Config API"""
        response = requests.get(f"{ADMIN_ENDPOINT}/config")
        self.assertEqual(response.status_code, 200)
        config = response.json()
        self.assertIn("server", config)
        self.assertIn("vendors", config)
        
    def test_07_admin_vendor_status(self):
        """Test Admin Vendor Status API"""
        response = requests.get(f"{ADMIN_ENDPOINT}/vendors")
        self.assertEqual(response.status_code, 200)
        vendors = response.json()
        self.assertTrue(len(vendors) > 0)
        
        # Check specific vendor status
        vendor_id = vendors[0]["id"]
        response = requests.get(f"{ADMIN_ENDPOINT}/vendors/{vendor_id}/status")
        self.assertEqual(response.status_code, 200)
        status = response.json()
        self.assertIn("status", status)

    def test_08_rate_limit_concurrency(self):
        """Test Concurrency Rate Limiting (Expect some 429s or success depending on config)"""
        # Our config allows 100 RPS, so this test might not trigger 429 unless we push harder 
        # or reduce the limit in the test setup.
        # But we can verify that parallel requests work.
        
        def make_request():
            payload = {
                "model": "gpt-3.5-turbo",
                "messages": [{"role": "user", "content": "Load test"}]
            }
            headers = {"Authorization": "Bearer test-token"}
            try:
                resp = requests.post(OPENAI_ENDPOINT, json=payload, headers=headers, timeout=5)
                return resp.status_code
            except Exception as e:
                return 0

        threads = []
        results = []
        
        def worker():
            code = make_request()
            results.append(code)

        # Launch 20 concurrent requests
        for _ in range(20):
            t = threading.Thread(target=worker)
            threads.append(t)
            t.start()
            
        for t in threads:
            t.join()
            
        success_count = results.count(200)
        print(f"\n[Load Test] 20 Requests: {success_count} success, {len(results)-success_count} failures")
        self.assertTrue(success_count > 0)

if __name__ == "__main__":
    # Wait for server to start if needed
    print("Waiting for proxy server to be ready...")
    for _ in range(10):
        try:
            requests.get(f"{PROXY_URL}/health", timeout=1)
            print("Proxy server is ready!")
            break
        except:
            time.sleep(1)
            
    unittest.main()
