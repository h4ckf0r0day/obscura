# Python SDK

```python
from obscura_next.client import ObscuraClient
with ObscuraClient() as client:
    client.create_session()
    r = client.navigate("https://example.com")
    print(r.success)
```

Async client `AsyncObscuraClient` available. Worker binary resolved via OBSCURA_WORKER_BIN or build dirs.
