/** Portable join and connection state supplied by idle-peer-state bindings. */
export interface JoinState {
  readonly generation: number;
  readonly enabled: boolean;
  enable(generation: number): boolean;
  is_current(generation: number): boolean;
  retire(): void;
}

export interface ConnectionState {
  readonly generation: number;
  readonly status: string;
  readonly retry_delay_ms: number;
  begin(): number;
  stop(): void;
  waiting(generation: number): void;
  expired(generation: number): void;
  ready(generation: number): void;
  authenticating(generation: number): void;
  progress(generation: number, json: string): void;
}

export type StateFactory = {
  joinState(): JoinState;
  connectionState(): ConnectionState;
};
