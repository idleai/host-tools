use std::sync::{Arc, atomic::Ordering};

use idle_coordination::{
    Error, Result,
    discovery::Advertisement,
    engine::{Engine, ScopeChoice},
    invitation::{Invitation, JoinRequest, SavedSharing, encode},
    peer::PeerCoordinator,
    persistence::{FilePersistence, Persistence},
};
use idle_history::connection::ConnectionStatus;
use tokio_util::sync::CancellationToken;

use super::support::{
    self, Memory, NOW, TestClock, record_count,
    relay::{Credential, Relay},
    seed, until,
};

struct Pair {
    _left: tempfile::TempDir,
    _right: tempfile::TempDir,
    left_engine: Engine,
    right_engine: Engine,
    left: PeerCoordinator,
    right: PeerCoordinator,
    relay: Arc<Relay>,
    credentials: Arc<Credential>,
    left_storage: Arc<FilePersistence>,
    right_storage: Arc<Memory>,
}

impl Pair {
    async fn new() -> Result<Self> {
        let left = tempfile::tempdir()?;
        let right = tempfile::tempdir()?;
        let left_engine = support::engine(left.path());
        let right_engine = support::engine(right.path());
        seed(&left_engine.chain, 1, 1, b"left's original record")?;
        seed(&right_engine.chain, 2, 1, b"right's original record")?;
        let relay = Relay::new(Arc::new(TestClock::default()));
        let credentials = Arc::new(Credential::default());
        let right_storage = Arc::new(Memory::default());
        let left_storage = support::storage(left.path())?;
        let left_peer = relay
            .coordinator(
                left_engine.clone(),
                left_storage.clone(),
                credentials.clone(),
            )
            .await?;
        let right_peer = relay
            .coordinator(
                right_engine.clone(),
                right_storage.clone(),
                credentials.clone(),
            )
            .await?;
        Ok(Self {
            _left: left,
            _right: right,
            left_engine,
            right_engine,
            left: left_peer,
            right: right_peer,
            relay,
            credentials,
            left_storage,
            right_storage,
        })
    }

    async fn connect(&mut self, choice: ScopeChoice) -> Result<String> {
        let invitation = self
            .left
            .host_history(
                &self.right.join_request()?,
                choice,
                &CancellationToken::new(),
            )
            .await?;
        self.right
            .join_history(&invitation, ScopeChoice::All, &CancellationToken::new())
            .await?;
        Ok(invitation)
    }

    async fn converged(&self, left: usize, right: usize) {
        until(|| {
            record_count(&self.left_engine) == Ok(left)
                && record_count(&self.right_engine) == Ok(right)
        })
        .await;
    }

    async fn stop(&mut self) -> support::TestResult {
        self.right.stop().await?;
        self.left.stop().await?;
        equal!(
            self.relay.active_clients.load(Ordering::SeqCst),
            0,
            "stop must await every client transport"
        )?;
        ensure!(
            self.relay
                .resources
                .lock()
                .map_err(|_error| Error::Storage)?
                .is_empty(),
            "stop must delete exactly the owned host resources"
        )?;
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simultaneous_large_blobs_converge_through_bounded_streams() -> support::TestResult {
    let mut pair = Pair::new().await?;
    seed(&pair.left_engine.chain, 1, 2, &vec![41_u8; 1_048_576])?;
    seed(&pair.right_engine.chain, 2, 2, &vec![42_u8; 1_048_576])?;
    let _invitation = pair.connect(ScopeChoice::All).await?;
    until(|| {
        [&pair.left, &pair.right].iter().all(|peer| {
            peer.status().peers.iter().any(|connection| {
                connection.state == ConnectionStatus::Live
                    && connection
                        .progress
                        .as_ref()
                        .is_some_and(|progress| progress.blobs >= 2)
            })
        })
    })
    .await;
    pair.converged(4, 4).await;
    equal!(
        pair.relay.connect_attempts.load(Ordering::SeqCst),
        1,
        "both directions must complete without a timeout or reconnect"
    )?;
    pair.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_drains_connections_on_unreadable_or_corrupt_journal() -> support::TestResult {
    let mut pair = Pair::new().await?;
    let _invitation = pair.connect(ScopeChoice::All).await?;
    pair.converged(2, 2).await;
    pair.right_storage
        .fail_stop_read
        .store(true, Ordering::SeqCst);
    equal!(
        pair.right.stop().await,
        Err(Error::Storage),
        "journal read failures must be reported"
    )?;
    ensure!(
        !pair.right.status().enabled,
        "a failed Stop must retire the active generation"
    )?;
    equal!(
        pair.relay.active_clients.load(Ordering::SeqCst),
        0,
        "Stop must await existing transports despite the journal failure"
    )?;
    pair.right_storage
        .fail_stop_read
        .store(false, Ordering::SeqCst);
    let _invitation = pair.connect(ScopeChoice::Keep).await?;
    until(|| pair.relay.active_clients.load(Ordering::SeqCst) == 1).await;
    pair.right_storage
        .compare_exchange("sharing-stop", None, Some(b"corrupt"))?;
    equal!(
        pair.right.stop().await,
        Err(Error::Invalid),
        "corrupt Stop data must remain visible"
    )?;
    ensure!(
        !pair.right.status().enabled,
        "decoding failures must also retire sharing"
    )?;
    equal!(
        pair.relay.active_clients.load(Ordering::SeqCst),
        0,
        "corrupt journal data cannot bypass transport draining"
    )?;
    pair.right_storage
        .compare_exchange("sharing-stop", Some(b"corrupt"), None)?;
    pair.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rust_peers_replicate_records_blobs_live_updates_and_recover_after_restart()
-> support::TestResult {
    let mut pair = Pair::new().await?;
    let invitation = pair.connect(ScopeChoice::All).await?;
    pair.converged(2, 2).await;
    until(|| {
        pair.right
            .status()
            .peers
            .iter()
            .any(|peer| peer.state == ConnectionStatus::Live)
    })
    .await;
    let content = vec![42_u8; 1_048_576];
    seed(&pair.left_engine.chain, 1, 2, &content)?;
    pair.converged(3, 3).await;
    until(|| {
        pair.right.status().peers.iter().any(|peer| {
            peer.progress
                .as_ref()
                .is_some_and(|progress| progress.blobs >= 2)
        })
    })
    .await;
    let scope = pair.right_engine.scope().await?;
    let identity = pair.right_engine.identity().await?;
    pair.right.suspend().await?;
    let saved = pair.right_storage.load("saved-sharing")?;
    ensure!(
        saved.is_some(),
        "suspension must retain the existing saved format"
    )?;
    let mut restarted = pair
        .relay
        .coordinator(
            pair.right_engine.clone(),
            pair.right_storage.clone(),
            pair.credentials.clone(),
        )
        .await?;
    restarted.resume().await?;
    std::mem::swap(&mut pair.right, &mut restarted);
    drop(restarted);
    pair.left.reconnect().await?;
    seed(&pair.left_engine.chain, 1, 3, b"after restart")?;
    pair.converged(4, 4).await;
    equal!(
        pair.right_engine.scope().await?,
        scope,
        "restart cannot recreate or widen consent"
    )?;
    equal!(
        pair.right_engine.identity().await?,
        identity,
        "reconnect must retain persistent device identity"
    )?;
    ensure!(
        pair.right.inspect_invitation(&invitation).is_ok(),
        "existing invitation format must remain valid"
    )?;
    pair.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn from_now_scope_survives_reconnect_and_cross_space_invitation_is_rejected()
-> support::TestResult {
    let mut pair = Pair::new().await?;
    let encoded = pair.connect(ScopeChoice::FromNow).await?;
    pair.converged(2, 1).await;
    let scope = pair.left_engine.scope().await?;
    seed(&pair.left_engine.chain, 1, 2, b"approved after cutoff")?;
    pair.converged(3, 2).await;
    pair.left.reconnect().await?;
    pair.right.reconnect().await?;
    equal!(
        pair.left_engine.scope().await?,
        scope,
        "keep/reconnect must preserve the original cutoff revision"
    )?;
    let mut changed = pair.right.inspect_invitation(&encoded)?;
    changed.space = "different-space".into();
    equal!(
        pair.right
            .join_history(
                &encode(&changed)?,
                ScopeChoice::All,
                &CancellationToken::new()
            )
            .await,
        Err(Error::Conflict),
        "another space cannot rebind an existing chain"
    )?;
    equal!(
        record_count(&pair.right_engine)?,
        2,
        "previously excluded local history must remain private"
    )?;
    pair.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_uses_backoff_and_revocation_stops_live_transfers() -> support::TestResult {
    let mut pair = Pair::new().await?;
    pair.relay.fail_connect.store(true, Ordering::SeqCst);
    let _invitation = pair.connect(ScopeChoice::All).await?;
    until(|| pair.relay.connect_attempts.load(Ordering::SeqCst) >= 2).await;
    ensure!(
        pair.relay.connect_attempts.load(Ordering::SeqCst) < 8,
        "transport failures must back off instead of busy-looping"
    )?;
    pair.relay.fail_connect.store(false, Ordering::SeqCst);
    pair.converged(2, 2).await;
    let guest = pair.right_engine.identity().await?;
    pair.left.revoke(&guest.fingerprint).await?;
    until(|| pair.relay.active_clients.load(Ordering::SeqCst) == 0).await;
    seed(&pair.left_engine.chain, 1, 2, b"private after revocation")?;
    tokio::time::sleep(std::time::Duration::from_millis(1700)).await;
    equal!(
        record_count(&pair.right_engine)?,
        2,
        "a revoked certificate must not receive later records"
    )?;
    // The remote failure is observable through status; explicit stop still performs cleanup.
    let _stopped = pair.right.stop().await;
    pair.left.stop().await?;
    equal!(
        pair.relay.active_clients.load(Ordering::SeqCst),
        0,
        "revoked workers must release transport resources"
    )?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simultaneous_hosting_collapses_duplicate_authenticated_edges() -> support::TestResult {
    let mut pair = Pair::new().await?;
    let _invitation = pair.connect(ScopeChoice::All).await?;
    pair.converged(2, 2).await;
    let reciprocal = pair
        .right
        .host_history(
            &pair.left.join_request()?,
            ScopeChoice::Keep,
            &CancellationToken::new(),
        )
        .await?;
    pair.left
        .join_history(&reciprocal, ScopeChoice::Keep, &CancellationToken::new())
        .await?;
    until(|| {
        pair.relay.active_clients.load(Ordering::SeqCst) == 1
            && pair.left.status().peers.len() == 1
            && pair.right.status().peers.len() == 1
    })
    .await;
    seed(&pair.right_engine.chain, 2, 2, b"one authenticated route")?;
    pair.converged(3, 3).await;
    pair.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approved_renewal_keeps_scope_and_rejects_new_tunnels_without_owner_fallback()
-> support::TestResult {
    let mut pair = Pair::new().await?;
    let encoded = pair.connect(ScopeChoice::All).await?;
    pair.converged(2, 2).await;
    let old = pair.right.inspect_invitation(&encoded)?;
    let mut renewed = old.clone();
    renewed.connect_token = support::token(NOW.saturating_add(7_200_000));
    renewed.expires_at = NOW.saturating_add(7_200_000);
    *pair.credentials.renewal.lock().expect("renewal adapter") = Some(renewed.clone());
    pair.relay
        .clock
        .0
        .store(NOW.saturating_add(3_550_000), Ordering::SeqCst);
    pair.right.reconnect().await?;
    until(|| pair.credentials.renewals.load(Ordering::SeqCst) > 0).await;
    until(|| {
        pair.right_storage
            .load("saved-sharing")
            .ok()
            .flatten()
            .and_then(|bytes| serde_json::from_slice::<SavedSharing>(&bytes).ok())
            .is_some_and(|saved| saved.peers.first() == Some(&renewed))
    })
    .await;
    equal!(
        pair.credentials.management_calls.load(Ordering::SeqCst),
        0,
        "guest renewal must not request owner credentials"
    )?;
    let mut widened = renewed.clone();
    widened.endpoint.tunnel_id = "unapproved-tunnel".into();
    equal!(
        renewed.validate_renewal(
            &widened,
            &pair.right_engine.identity().await?,
            NOW.saturating_add(3_550_000)
        ),
        Err(Error::Forbidden),
        "renewal cannot silently add a resource grant"
    )?;
    equal!(
        pair.right_engine
            .scope()
            .await?
            .expect("active consent")
            .mode,
        "all",
        "credential renewal must not change engine scope"
    )?;
    pair.stop().await
}

#[tokio::test]
async fn cancelled_host_and_failed_stop_remain_disabled_across_restart() -> support::TestResult {
    let mut pair = Pair::new().await?;
    pair.relay.block_host.store(true, Ordering::SeqCst);
    let cancel = CancellationToken::new();
    let cancelled = cancel.clone();
    let join_request = pair.right.join_request()?;
    let pending = pair
        .left
        .host_history(&join_request, ScopeChoice::All, &cancel);
    let signal = async {
        until(|| pair.relay.host_attempts.load(Ordering::SeqCst) > 0).await;
        cancelled.cancel();
    };
    let (result, ()) = tokio::join!(pending, signal);
    equal!(
        result,
        Err(Error::Cancelled),
        "host cancellation must be observed while management is pending"
    )?;
    ensure!(
        !pair.left.status().enabled,
        "late host work must not re-enable a retired generation"
    )?;
    pair.relay.block_host.store(false, Ordering::SeqCst);
    let _invitation = pair.connect(ScopeChoice::Keep).await?;
    pair.converged(2, 2).await;
    pair.relay.fail_remove.store(true, Ordering::SeqCst);
    equal!(
        pair.left.stop().await,
        Err(Error::Transport),
        "failed cloud deletion must remain visible to the caller"
    )?;
    equal!(
        pair.left.resume().await,
        Err(Error::Invalid),
        "stopped sharing must not silently resume"
    )?;
    ensure!(
        pair.left_storage.load("sharing-stop")?.is_some(),
        "failed deletion must retain a durable Stop intent"
    )?;
    pair.relay.fail_remove.store(false, Ordering::SeqCst);
    pair.left = pair
        .relay
        .coordinator(
            pair.left_engine.clone(),
            pair.left_storage.clone(),
            pair.credentials.clone(),
        )
        .await?;
    ensure!(
        !pair.left.status().enabled,
        "reopening must finish Stop without resuming sharing"
    )?;
    ensure!(
        pair.left_storage.load("sharing-stop")?.is_none(),
        "successful recovery retires the durable Stop intent"
    )?;
    pair.right.stop().await?;
    ensure!(
        pair.relay
            .resources
            .lock()
            .expect("relay resources")
            .is_empty(),
        "retry must complete the journaled resource deletion"
    )?;
    Ok(())
}

#[tokio::test]
async fn expired_grants_without_renewal_stop_without_changing_consent() -> support::TestResult {
    let mut pair = Pair::new().await?;
    let _invitation = pair.connect(ScopeChoice::FromNow).await?;
    let scope = pair.right_engine.scope().await?;
    pair.relay
        .clock
        .0
        .store(NOW.saturating_add(3_600_001), Ordering::SeqCst);
    pair.right.reconnect().await?;
    until(|| {
        pair.right
            .status()
            .peers
            .iter()
            .any(|peer| peer.state == ConnectionStatus::Expired)
    })
    .await;
    let attempts = pair.relay.connect_attempts.load(Ordering::SeqCst);
    tokio::time::sleep(std::time::Duration::from_millis(1800)).await;
    equal!(
        pair.relay.connect_attempts.load(Ordering::SeqCst),
        attempts,
        "expired grants must not retry connection authentication"
    )?;
    equal!(
        pair.right_engine.scope().await?,
        scope,
        "expiry must preserve the exact consent boundary"
    )?;
    equal!(
        pair.credentials.management_calls.load(Ordering::SeqCst),
        0,
        "guest expiry must never fall back to owner credentials"
    )?;
    pair.stop().await
}

#[tokio::test]
async fn uncertain_saved_state_cancels_hosting_and_outbound_workers() -> support::TestResult {
    let mut pair = Pair::new().await?;
    let _invitation = pair.connect(ScopeChoice::All).await?;
    pair.converged(2, 2).await;
    let _reciprocal = pair
        .right
        .host_history(
            &pair.left.join_request()?,
            ScopeChoice::Keep,
            &CancellationToken::new(),
        )
        .await?;
    let saved: SavedSharing = serde_json::from_slice(
        &pair
            .right_storage
            .load("saved-sharing")?
            .ok_or(Error::Storage)?,
    )?;
    let host = saved.host.ok_or(Error::Invalid)?;
    pair.right_storage.uncertain.store(true, Ordering::SeqCst);
    equal!(
        pair.right
            .revoke(&pair.left_engine.identity().await?.fingerprint)
            .await,
        Err(Error::Storage),
        "uncertain persistence must be reported"
    )?;
    until(|| {
        pair.relay.hosts.lock().is_ok_and(|hosts| {
            hosts
                .get(&host.tunnel_id)
                .is_some_and(tokio::sync::mpsc::Sender::is_closed)
        }) && pair.relay.active_clients.load(Ordering::SeqCst) == 0
    })
    .await;
    ensure!(
        !pair.right.status().enabled && !pair.right.status().hosting,
        "a fault must disable the entire hosting generation"
    )?;
    pair.right.suspend().await?;
    pair.right = pair
        .relay
        .coordinator(
            pair.right_engine.clone(),
            pair.right_storage.clone(),
            pair.credentials.clone(),
        )
        .await?;
    pair.stop().await
}

#[tokio::test]
async fn cleanup_errors_are_returned_after_generation_retirement() -> support::TestResult {
    let mut pair = Pair::new().await?;
    let _invitation = pair.connect(ScopeChoice::All).await?;
    pair.converged(2, 2).await;
    pair.relay.fail_close.store(true, Ordering::SeqCst);
    equal!(
        pair.left.suspend().await,
        Err(Error::Transport),
        "retiring callbacks must not swallow transport cleanup failures"
    )?;
    ensure!(
        !pair.left.status().hosting,
        "failed cleanup cannot leave status claiming a live host"
    )?;
    pair.stop().await
}

#[tokio::test]
async fn invitations_and_public_discovery_keep_existing_shapes_and_limits() -> support::TestResult {
    let mut pair = Pair::new().await?;
    let request = pair.right.join_request()?;
    let guest = JoinRequest::parse(&request)?.device;
    let encoded = pair.connect(ScopeChoice::All).await?;
    let invitation = Invitation::parse(&encoded, &guest, NOW)?;
    let json = serde_json::to_value(&invitation)?;
    ensure!(
        json.get("connectToken").is_some() && json.get("expiresAt").is_some(),
        "invitations must retain TypeScript field names"
    )?;
    ensure!(
        !format!("{invitation:?}").contains(&invitation.connect_token.0),
        "debug output must redact bearer grants"
    )?;
    let mut foreign = invitation.clone();
    foreign.endpoint.client_relay_uri = "wss://foreign.example/connect".into();
    equal!(
        Invitation::parse(&encode(&foreign)?, &guest, NOW),
        Err(Error::Invalid),
        "invitation URLs cannot redirect a bearer grant off the relay"
    )?;
    let mut incompatible = invitation.clone();
    incompatible.version = 2;
    equal!(
        Invitation::parse(&encode(&incompatible)?, &guest, NOW),
        Err(Error::Version),
        "unknown versions must request coordinated upgrades"
    )?;
    let advertisement: Advertisement = pair
        .left
        .describe(&CancellationToken::new())
        .await?
        .expect("active host descriptor");
    ensure!(
        !serde_json::to_string(&advertisement)?.contains(&invitation.connect_token.0),
        "public directory data must contain no connect token"
    )?;
    ensure!(
        advertisement.name().starts_with("EDITCHAIN_PEER_"),
        "discovery must retain the existing variable namespace"
    )?;
    pair.stop().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_migration_keeps_identity_consent_and_stop_wins_over_a_lost_ack() -> support::TestResult
{
    let mut pair = Pair::new().await?;
    let encoded = pair.connect(ScopeChoice::FromNow).await?;
    let identity = pair.right_engine.identity().await?;
    let scope = pair.right_engine.scope().await?;
    let approved = pair
        .right_engine
        .devices(&pair.right.status().space.ok_or(Error::Invalid)?)
        .await?;
    let saved: SavedSharing = serde_json::from_slice(
        &pair
            .right_storage
            .load("saved-sharing")?
            .ok_or(Error::Invalid)?,
    )?;
    pair.right.suspend().await?;
    let storage = Arc::new(Memory::default());
    let mut migrated = pair
        .relay
        .coordinator(
            pair.right_engine.clone(),
            storage.clone(),
            pair.credentials.clone(),
        )
        .await?;
    let mut changed = saved.clone();
    changed.peers.first_mut().ok_or(Error::Invalid)?.guest = "wrong-device".into();
    equal!(
        migrated.import_saved(changed).await,
        Err(Error::Forbidden),
        "import cannot readdress a grant"
    )?;
    migrated.import_saved(saved.clone()).await?;
    ensure!(
        !migrated.status().enabled,
        "import never grants new approval or enables sharing"
    )?;
    migrated.import_saved(saved.clone()).await?;
    migrated.resume().await?;
    equal!(
        pair.right_engine.identity().await?,
        identity,
        "migration keeps the exact device key"
    )?;
    equal!(
        pair.right_engine.scope().await?,
        scope,
        "migration cannot reset the outgoing cutoff"
    )?;
    equal!(
        pair.right_engine.devices(&saved.space).await?,
        approved,
        "migration keeps existing device approvals"
    )?;
    let host = migrated.inspect_invitation(&encoded)?.host;
    migrated.revoke(&host.fingerprint).await?;
    migrated.import_saved(saved.clone()).await?;
    ensure!(
        pair.right_engine.devices(&saved.space).await?.is_empty(),
        "a retried migration cannot reenroll a revoked device"
    )?;
    migrated.stop().await?;
    migrated.import_saved(saved).await?;
    equal!(
        migrated.resume().await,
        Err(Error::Invalid),
        "a lost import acknowledgement cannot undo Stop"
    )?;
    ensure!(
        storage.load("saved-sharing")?.is_none(),
        "Stop removes native resumption"
    )?;
    pair.left.stop().await?;
    Ok(())
}

#[derive(Debug, Default)]
struct UnavailableDirectory {
    failed: std::sync::atomic::AtomicBool,
    removals: std::sync::atomic::AtomicU64,
}

#[async_trait::async_trait]
impl idle_coordination::discovery::Directory for UnavailableDirectory {
    async fn read(
        &self,
        _space: &str,
        _now: u64,
        _cancel: &CancellationToken,
    ) -> Result<Vec<Advertisement>> {
        if self.failed.load(Ordering::SeqCst) {
            Err(Error::Transport)
        } else {
            Ok(Vec::new())
        }
    }
    async fn publish(
        &self,
        _value: &Advertisement,
        _now: u64,
        _cancel: &CancellationToken,
    ) -> Result<()> {
        if self.failed.load(Ordering::SeqCst) {
            Err(Error::Transport)
        } else {
            Ok(())
        }
    }
    async fn remove(&self, _value: &Advertisement, _cancel: &CancellationToken) -> Result<()> {
        let _count = self.removals.fetch_add(1, Ordering::SeqCst);
        if self.failed.load(Ordering::SeqCst) {
            Err(Error::Transport)
        } else {
            Ok(())
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn directory_failure_keeps_peer_streams_and_retries_only_its_own_public_entry()
-> support::TestResult {
    let mut pair = Pair::new().await?;
    let _invitation = pair.connect(ScopeChoice::All).await?;
    pair.converged(2, 2).await;
    let adapter = Arc::new(UnavailableDirectory::default());
    let mut directory = idle_coordination::discovery::DirectorySync::new(adapter.clone());
    directory
        .refresh(&mut pair.left, NOW, &CancellationToken::new())
        .await?;
    adapter.failed.store(true, Ordering::SeqCst);
    equal!(
        directory
            .refresh(&mut pair.left, NOW, &CancellationToken::new())
            .await,
        Err(Error::Transport),
        "directory failure remains separate from peer recovery"
    )?;
    ensure!(
        pair.left.status().enabled
            && pair.left.status().peers.iter().any(|peer| peer
                .progress
                .as_ref()
                .is_some_and(|progress| progress.accepted)),
        "discovery outages leave authenticated streams alone"
    )?;
    equal!(
        directory.stop(&CancellationToken::new()).await,
        Err(Error::Transport),
        "failed withdrawal remains retryable"
    )?;
    adapter.failed.store(false, Ordering::SeqCst);
    directory.stop(&CancellationToken::new()).await?;
    directory.stop(&CancellationToken::new()).await?;
    equal!(
        adapter.removals.load(Ordering::SeqCst),
        2,
        "successful withdrawal retires the exact published entry"
    )?;
    pair.left.stop().await?;
    pair.right.stop().await?;
    Ok(())
}
