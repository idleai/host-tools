'use strict';

// Integration peer using the existing production TypeScript bridge and worker IPC.
const assert = require('node:assert/strict');
const net = require('node:net');
const { once } = require('node:events');
const { NativeWorker, PeerBridge } = require('../packages/history-runtime/dist/native.js');
const { parseInvitation, savedInvitation, encodeInvitation } = require('../packages/history-runtime/dist/invitation.js');

async function main() {
  const chunks = [];
  for await (const bytes of process.stdin) chunks.push(bytes);
  const config = JSON.parse(Buffer.concat(chunks).toString('utf8'));
  const invitation = parseInvitation(config.invitation, config.now);
  assert.deepEqual(savedInvitation(invitation, config.now), invitation);
  assert.deepEqual(parseInvitation(encodeInvitation(invitation), config.now), invitation);
  const worker = new NativeWorker(config.binary);
  try {
    const identity = await worker.request({ type: 'identity', device_dir: config.device });
    assert.equal(identity.fingerprint, invitation.guest);
    assert.deepEqual(await worker.request({ type: 'verify', certificate: invitation.host.certificate }), invitation.host);
    await worker.request({ type: 'configure', chain_dir: config.chain, space: invitation.space, backfill: true });
    await worker.request({ type: 'approve', chain_dir: config.chain, space: invitation.space, certificate: invitation.host.certificate });
  } finally { worker.stop(); }
  const stream = net.connect({ host: '127.0.0.1', port: config.port });
  await once(stream, 'connect');
  let bridge, timer;
  try {
    const converged = new Promise((resolve, reject) => {
      timer = setTimeout(() => reject(new Error('TypeScript peer did not converge')), 20_000);
      bridge = new PeerBridge(config.binary, stream, { chain_dir: config.chain, device_dir: config.device,
        space: invitation.space, remote: invitation.host.certificate }, (progress, device) => {
        assert.equal(device?.fingerprint ?? invitation.host.fingerprint, invitation.host.fingerprint);
        if (progress.records === 1 && progress.blobs === 1 && progress.sent_records === 1 && progress.sent_blobs === 1) resolve(progress);
      }, error => { if (error) reject(error); }, 100);
    });
    await bridge.start();
    const progress = await converged;
    process.stdout.write(JSON.stringify({ received_records: progress.records, received_blobs: progress.blobs }));
  } finally { clearTimeout(timer); bridge?.stop(); stream.destroy(); }
}

main().catch(error => { process.stderr.write(`${error.message}\n`); process.exitCode = 1; });
