export interface SessionConfig {
  proxyUrl?: string;
  stealth?: boolean;
  userAgent?: string;
  allowFileAccess?: boolean;
  allowPrivateNetwork?: boolean;
  timeoutMs?: number;
}

export interface ActionResult<T = unknown> {
  action: string;
  success: boolean;
  payload?: T;
  elapsedMs: number;
  error?: string;
}

export interface SemanticNode {
  id: string;
  tag: string;
  text: string;
  selector: string;
  role?: string;
  isInteractive: boolean;
  attributes?: Record<string, string>;
  children?: SemanticNode[];
}

export type WaitUntil = 'load' | 'domcontentloaded' | 'networkidle0' | 'networkidle2';
