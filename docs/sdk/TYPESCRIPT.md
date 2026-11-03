# TypeScript SDK

```ts
import { ObscuraClient } from '@obscura-next/client';
const client = new ObscuraClient();
await client.start();
await client.createSession();
const r = await client.navigate('https://example.com');
await client.close();
```
