import { spawn, ChildProcess } from 'child_process';
import * as readline from 'readline';
import { ActionResult, SessionConfig, WaitUntil } from './types';

export * from './types';

export class ObscuraClient {
  private binaryPath: string;
  private proc: ChildProcess | null = null;
  private rl: readline.Interface | null = null;
  private pendingResolvers: Array<(value: unknown) => void> = [];

  constructor(binaryPath = 'obscura') {
    this.binaryPath = binaryPath;
  }

  public async start(): Promise<void> {
    this.proc = spawn(this.binaryPath, ['worker'], {
      stdio: ['pipe', 'pipe', 'pipe'],
    });

    if (!this.proc.stdout || !this.proc.stdin) {
      throw new Error('Failed to acquire worker stdio handles');
    }

    this.rl = readline.createInterface({ input: this.proc.stdout });
    this.rl.on('line', (line: string) => {
      const resolver = this.pendingResolvers.shift();
      if (resolver) {
        try {
          resolver(JSON.parse(line));
        } catch (e) {
          resolver({ ok: false, error: String(e) });
        }
      }
    });
  }

  private sendCommand<T = unknown>(cmd: Record<string, unknown>): Promise<T> {
    return new Promise((resolve, reject) => {
      if (!this.proc || !this.proc.stdin) {
        return reject(new Error('Worker not running'));
      }
      this.pendingResolvers.push(resolve as (value: unknown) => void);
      this.proc.stdin.write(JSON.stringify(cmd) + '\n');
    });
  }

  public async navigate(url: string, waitUntil: WaitUntil = 'load'): Promise<ActionResult> {
    const t0 = Date.now();
    const resp = await this.sendCommand<{ ok: boolean; result?: unknown; error?: string }>({
      cmd: 'navigate',
      url,
      waitUntil,
    });
    return {
      action: 'navigate',
      success: resp.ok,
      payload: resp.result,
      elapsedMs: Date.now() - t0,
      error: resp.error,
    };
  }

  public async evaluate(expression: string): Promise<ActionResult> {
    const t0 = Date.now();
    const resp = await this.sendCommand<{ ok: boolean; result?: unknown; error?: string }>({
      cmd: 'evaluate',
      expression,
    });
    return {
      action: 'evaluate',
      success: resp.ok,
      payload: resp.result,
      elapsedMs: Date.now() - t0,
      error: resp.error,
    };
  }

  public async dump(format: 'html' | 'text' = 'html'): Promise<string> {
    const cmd = format === 'html' ? 'dump_html' : 'dump_text';
    const resp = await this.sendCommand<{ ok: boolean; result?: string }>({ cmd });
    return resp.result || '';
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
