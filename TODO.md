# deepseek error
error: InternalError - LLM error during tool loop: HTTP 400: {"error":{"message":"The `reasoning_content` in the thinking mode must be passed back to the API.","type":"invalid_request_error","param":null,"code":"invalid_request_error"}}

# moonshot error
error: InternalError - LLM error during tool loop: HTTP 400: {"error":{"message":"Error from provider: Provider returned error","code":400,"metadata":{"raw":"{\"error\":{\"message\":\"thinking is enabled but reasoning_content is missing in assistant tool call message at index 4\",\"type\":\"invalid_request_error\"}}","provider_name":"Moonshot AI","is_byok":true}},"user_id":"user_2z4xm5LomaIHfsnVqMhFsWrVrGY"}

---

looks like we are not doing tool use in openai_compatible apis correctly at all.
