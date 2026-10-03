# Application activity conversion

```sh
idle-history-tools --chain new import --manifest sources.json --include-thinking
idle-history-tools --chain old convert --destination converted
editchain --chain converted history --kind Tool --limit 20
editchain --chain converted history --session SESSION_PREFIX --kind Message
editchain --chain converted history --item ITEM_PREFIX
editchain --chain converted search --kind Message -- import
editchain --chain converted content OPERATION_PREFIX --field '{"MessageBlock":0}'
```

The converter folds recognized bookkeeping into direct fields, combines a call's
arguments and output when one original snapshot contains both, and retains each
physical input record once as Original. It does not remove distinct source copies.
Recognized file-path notes move into File.name. Unsupported source structures remain
available through their original bytes. Original records are independently indexed;
select an activity type to keep raw input out of a study view.

Claude's ordinary and logical parent relationships remain separate Link records.
Their targets use session-scoped item IDs, so links remain useful when their
endpoints arrive later. Commit records and operation-to-Git Links feed the same Git
projection as older records. Buffered import storage supports reading captured
source blobs before conversion; unreadable source blobs fail the import before
cursor acceptance.

`--legacy` selects the previous capture schema and cursors. `--raw-only` retains
Original records only and uses separate cursors, allowing later normalization to
backfill activities. These options do not rewrite existing chains.

Physical migration always writes a separate destination. Changed records get new
operation IDs; old IDs and folded-record IDs remain searchable through mappings
stored in the converted records. The index can rebuild those mappings. Source
segments are retained byte-for-byte under `migration-v1/original`. Unknown binary
records and conflicting old representations retain their exact bytes. Legacy
ChainStart records remain initialization data outside the new activity enum.
References to these unchanged records retain their original IDs.

Fresh capture and physical migration use different deterministic ID namespaces,
because only migration embeds the complete folded-address mapping. A migrated
chain retains its converter marker and accepted cursor offsets; the namespace can
also be recovered from converted records after an archive restore. Do not change
converter semantics without changing its identity contract. Migration currently
refuses chains with configured sharing rules, leaving the source intact, until
those rules can be translated across folded records.
Sources that already contain schema-three records are also rejected: use that
chain directly so a repeated conversion cannot change future import identities.

The corrected converter contract is `activity-schema3-v3`. It isolates conflicted
identities before collecting metadata, preserves explicit occurrence, containment,
and tool-result links, and retains Claude reasoning categories and per-call
recorded error/success flags. It uses new conversion ID namespaces to avoid
assigning changed record bytes to an existing ID. EC03
framing and the canonical ID format are unchanged. Chains created by the earlier
v1 or v2 converters remain readable, but new imports into them are rejected.
To obtain a corrected chain, reimport the original sources into a new destination, or migrate
the original pre-schema-three chain again. A physical migration retains that
original under `migration-v1/original`. Keep the earlier chain for any activities
that were authored directly in it; this converter does not rewrite schema-three
records.

Engine indexes rebuild under `index-v3`.
Peer compatibility is version 5. Older binary operation schemas remain readable.
`Operation::view` supplies a current-vocabulary read adapter for old records;
`display_op` is a lossy bridge for older preview renderers and must not be persisted.
