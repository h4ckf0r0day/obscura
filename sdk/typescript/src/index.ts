import { spawn, ChildProcess } from 'child_process';
import * as readline from 'readline';
import * as path from 'path';
import * as fs from 'fs';
import { ActionResult, SessionConfig, WaitUntil } from './types';

export * from './types';

interface WorkerResponse {
  id?: string;
  version?: string;
  ok: boolean;
  result?: unknown;
  error?: { code: string; message: string; details?: unknown };
}

function resolveWorkerBinary(configured: string): string {
  // 1. Absolute path
  if (path.isAbsolute(configured) && fs.existsSync(configured)) {
    return configured;
  }

  // 2. Environment variable
  const envBin = process.env.OBSCURA_WORKER_BIN;
  if (envBin && fs.existsSync(envBin)) {
    return envBin;
  }

  // 3. Build directory candidates
  const candidates = [
    'target/debug/obscura-runtime-worker',
    'target/release/obscura-runtime-worker',
    'target/debug/obscura-worker',
    'target/release/obscura-worker',
    '../target/debug/obscura-runtime-worker',
    '../target/release/obscura-runtime-worker',
  ];
  for (const c of candidates) {
    const resolved = path.resolve(c);
    if (fs.existsSync(resolved)) {
      return resolved;
    }
  }

  return configured;
}

export class ObscuraClient {
  private binaryPath: string;
  private defaultSessionId: string;
  private proc: ChildProcess | null = null;
  private rl: readline.Interface | null = null;
  private pendingResolvers: Map<string, (value: WorkerResponse) => void> = new Map();
  private reqCounter = 0;

  constructor(binaryPath = 'obscura-runtime-worker', defaultSessionId?: string) {
    this.binaryPath = resolveWorkerBinary(binaryPath);
    this.defaultSessionId = defaultSessionId || `ts-session-${Date.now().toString(36)}`;
  }

  private nextId(): string {
    this.reqCounter++;
    return `ts-${this.reqCounter}`;
  }

  public async start(): Promise<void> {
    // Spawn worker binary directly — no subcommand needed
    this.proc = spawn(this.binaryPath, [], {
      stdio: ['pipe', 'pipe', 'pipe'],
    });

    if (!this.proc.stdout || !this.proc.stdin) {
      throw new Error('Failed to acquire worker stdio handles');
    }

    this.rl = readline.createInterface({ input: this.proc.stdout });
    this.rl.on('line', (line: string) => {
      try {
        const resp = JSON.parse(line) as WorkerResponse;
        if (resp.id) {
          const resolver = this.pendingResolvers.get(resp.id);
          if (resolver) {
            this.pendingResolvers.delete(resp.id);
            resolver(resp);
          }
        }
      } catch {
        // Malformed JSON — ignore
      }
    });

    this.proc.on('exit', () => {
      // Fail all pending requests
      for (const [id, resolver] of this.pendingResolvers) {
        resolver({ ok: false, error: { code: 'WORKER_TERMINATED', message: 'Worker process exited' } });
      }
      this.pendingResolvers.clear();
    });
  }

  private sendCommand<T = WorkerResponse>(cmd: Record<string, unknown>): Promise<T> {
    return new Promise((resolve, reject) => {
      if (!this.proc || !this.proc.stdin) {
        return reject(new Error('Worker not running'));
      }
      const id = cmd.id as string || this.nextId();
      cmd.id = id;
      this.pendingResolvers.set(id, resolve as unknown as (value: WorkerResponse) => void);
      this.proc.stdin.write(JSON.stringify(cmd) + '\n');
    });
  }

  public async ping(): Promise<WorkerResponse> {
    return this.sendCommand({ cmd: 'ping' });
  }

  public async health(): Promise<WorkerResponse> {
    return this.sendCommand({ cmd: 'health' });
  }

  public async createSession(sessionId?: string, config?: SessionConfig): Promise<WorkerResponse> {
    const sid = sessionId || this.defaultSessionId;
    const cmd: Record<string, unknown> = { cmd: 'session_create', session_id: sid };
    if (config) {
      cmd.config = config;
    }
    return this.sendCommand(cmd);
  }

  public async destroySession(sessionId?: string): Promise<WorkerResponse> {
    const sid = sessionId || this.defaultSessionId;
    return this.sendCommand({ cmd: 'session_destroy', session_id: sid });
  }

  public async navigate(url: string, sessionId?: string, waitUntil: WaitUntil = 'load', timeoutMs?: number): Promise<ActionResult> {
    const sid = sessionId || this.defaultSessionId;
    const t0 = Date.now();
    const cmd: Record<string, unknown> = {
      cmd: 'navigate',
      session_id: sid,
      url,
      waitUntil,
    };
    if (timeoutMs !== undefined) cmd.timeout_ms = timeoutMs;
    const resp = await this.sendCommand<WorkerResponse>(cmd);
    return {
      action: 'navigate',
      success: resp.ok,
      payload: resp.result,
      elapsedMs: Date.now() - t0,
      error: resp.error ? resp.error.message : undefined,
    };
  }

  public async evaluate(expression: string, sessionId?: string, awaitPromise = true, timeoutMs?: number): Promise<ActionResult> {
    const sid = sessionId || this.defaultSessionId;
    const t0 = Date.now();
    const cmd: Record<string, unknown> = {
      cmd: 'evaluate',
      session_id: sid,
      expression,
      await_promise: awaitPromise,
    };
    if (timeoutMs !== undefined) cmd.timeout_ms = timeoutMs;
    const resp = await this.sendCommand<WorkerResponse>(cmd);
    return {
      action: 'evaluate',
      success: resp.ok,
      payload: resp.result,
      elapsedMs: Date.now() - t0,
      error: resp.error ? resp.error.message : undefined,
    };
  }

  public async click(selector: string, sessionId?: string): Promise<ActionResult> {
    const sid = sessionId || this.defaultSessionId;
    const t0 = Date.now();
    const resp = await this.sendCommand<WorkerResponse>({ cmd: 'click', session_id: sid, selector });
    return { action: 'click', success: resp.ok, payload: resp.result, elapsedMs: Date.now() - t0, error: resp.error?.message };
  }

  public async fill(selector: string, value: string, sessionId?: string): Promise<ActionResult> {
    const sid = sessionId || this.defaultSessionId;
    const t0 = Date.now();
    const resp = await this.sendCommand<WorkerResponse>({ cmd: 'fill', session_id: sid, selector, value });
    return { action: 'fill', success: resp.ok, payload: resp.result, elapsedMs: Date.now() - t0, error: resp.error?.message };
  }

  public async typeText(selector: string, text: string, sessionId?: string): Promise<ActionResult> {
    const sid = sessionId || this.defaultSessionId;
    const t0 = Date.now();
    const resp = await this.sendCommand<WorkerResponse>({ cmd: 'type', session_id: sid, selector, text });
    return { action: 'type', success: resp.ok, payload: resp.result, elapsedMs: Date.now() - t0, error: resp.error?.message };
  }

  public async screenshot(sessionId?: string): Promise<WorkerResponse> {
    const sid = sessionId || this.defaultSessionId;
    return this.sendCommand({ cmd: 'screenshot', session_id: sid });
  }

  public async title(sessionId?: string): Promise<string> {
    const sid = sessionId || this.defaultSessionId;
    const resp = await this.sendCommand<WorkerResponse>({ cmd: 'title', session_id: sid });
    if (resp.ok && resp.result !== undefined && resp.result !== null) {
      if (typeof resp.result === 'string') return resp.result;
      if (typeof resp.result === 'object') {
        return (resp.result as Record<string, string>).title || '';
      }
    }
    return '';
  }

  public async dump(format: 'html' | 'text' = 'html', sessionId?: string): Promise<string> {
    const sid = sessionId || this.defaultSessionId;
    const resp = await this.sendCommand<WorkerResponse>({ cmd: 'dump', session_id: sid, format });
    if (resp.ok && resp.result !== undefined && resp.result !== null) {
      if (typeof resp.result === 'string') return resp.result;
      if (typeof resp.result === 'object') {
        return (resp.result as Record<string, string>).content || '';
      }
    }
    return '';
  }

  public async snapshot(sessionId?: string): Promise<WorkerResponse> {
    const sid = sessionId || this.defaultSessionId;
    return this.sendCommand({ cmd: 'snapshot', session_id: sid });
  }

  public async close(): Promise<void> {
    if (this.proc) {
      try {
        await this.sendCommand({ cmd: 'shutdown' });
      } catch {
        // ignore
      }
      if (this.proc.stdin) this.proc.stdin.end();
      this.proc.kill();
      this.proc = null;
    }
  }
}
