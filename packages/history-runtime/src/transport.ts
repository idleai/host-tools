import type { Duplex } from 'node:stream';
import type { Invitation, RelayEndpoint } from './invitation';
import { ProbeError } from './errors';

export type HostLease = { marker: string; tunnelId: string; clusterId: string };
export type HostTransport = {
  start(previous?: HostLease): Promise<void>;
  descriptor(): Promise<{ endpoint: RelayEndpoint; connectToken: string; expiresAt: number }>;
  lease(): HostLease;
  stop(): Promise<void>;
  suspend(): Promise<void>;
};
export type ClientTransport = {
  connect(invitation: Invitation): Promise<Duplex>;
  stop(): Promise<void>;
};

export function validateLease(input: unknown): HostLease {
  const value = input as Partial<HostLease> | null;
  if (!value || typeof value.marker !== 'string' || !/^(?:editchain-multiplayer|idle-relay)-[a-f0-9]{24}$/.test(value.marker) ||
    ![value.tunnelId, value.clusterId].every(id => typeof id === 'string' && /^[a-zA-Z0-9-]{1,128}$/.test(id))) {
    throw new ProbeError('Invalid saved hosting resource.');
  }
  return { marker: value.marker, tunnelId: value.tunnelId!, clusterId: value.clusterId! };
}
