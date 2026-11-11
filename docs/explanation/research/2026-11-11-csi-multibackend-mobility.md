# Kubernetes CSI, pod mobility and multi-backend mirroring for Lunix

> **Corrections (2026-11-11):** several facts in this note were checked against primary sources in [2026-11-11-fact-check.md](2026-11-11-fact-check.md) (ublk flag kernel versions, provider conditional-write support, prices, latencies, LSVD numbers). Where they disagree, the fact-check wins.

Date: 2026-11-11
Author: research agent (Sonnet)
Question: How should Lunix (a Rust ublk/io_uring block-device daemon over chunked, content-addressed, S3-backed storage) integrate with Kubernetes as a CSI driver (raw block and filesystem volumes, RWO), how should a stateful pod move between regions and clouds with a fenced single writer and a cold cache, and how should a volume be mirrored across N object-store backends that can be added and removed live? Includes what can be shared with Constellation's CSI crate, credentials handling, and a concrete "move a pod us-east -> ap-southeast" runbook.

Method note: web access was limited (search returned summaries, a few WebFetch pages). Facts from the CSI spec / kernel ublk / Constellation tree are from my knowledge plus local code reading; anything not re-checked this session is marked UNVERIFIED. Pricing numbers are list prices from memory and must be re-checked before they go into a product doc.

## Summary

- **Split the data path from the plugin.** The ublk server (`lunixd`, one process per node, or one per volume) must NOT live in the CSI node-plugin container. Ceph-CSI's rbd-nbd mode is the cautionary tale: the nbd userspace process runs inside the `csi-rbdplugin` pod, a plugin restart/upgrade kills it, the mount goes read-only/EIO, and upstream (ceph-csi #5068, Ceph tracker #51820) and Rook both say nbd/fuse mounters are "not recommended for production" because of this. Lunix must run `lunixd` as its own DaemonSet (separate pod, own lifecycle, `updateStrategy: OnDelete` or slow rolling) and the node plugin is a thin gRPC client of it over a hostPath unix socket.
- **ublk gives us a restart story that nbd lacks.** With `UBLK_F_USER_RECOVERY` the kernel keeps `/dev/ublkbN` alive, quiesces it when the server dies, and a new server re-attaches (`START_USER_RECOVERY` ... `END_USER_RECOVERY`). So even a `lunixd` crash/upgrade need not unmount the filesystem; I/O stalls (or is re-issued / failed per flag) rather than disappearing. Combine with Constellation's pattern (a state dir lock + explicit handoff) for zero-surprise upgrades. UNVERIFIED on Linux 7.0 exact semantics; a spike (K0-style) is required.
- **CSI shape:** `volume_mode: Block` is the natural primary mode (NodePublish = bind-mount/mknod of `/dev/ublkbN` at the target path file); `Filesystem` mode = Lunix does `mkfs` + `mount` in NodeStage (staging path) and bind-mount in NodePublish, like every block driver. Advertise `SINGLE_NODE_WRITER` and `SINGLE_NODE_SINGLE_WRITER`/`SINGLE_NODE_MULTI_WRITER` as appropriate (RWO + RWOP), `EXPAND_VOLUME` (online, node-side), snapshots (`CREATE_DELETE_SNAPSHOT`), clones (`CLONE_VOLUME`), `VOLUME_CONDITION`, `GET_VOLUME` and (for cross-AZ) `VolumeAccessibilityConstraints`. No attach step needed: `attachRequired: false` in the CSIDriver object.
- **A Lunix volume ID can be fully portable if it is "bucket set + volume uuid"** and all state is in S3. Static PV (`volumeHandle: lunix:v1:<volume-uuid>`, backend list in a per-PV/StorageClass secret or a `LunixVolumeBinding` CRD) works in any cluster with access to any one mirror. No cross-cluster controller needed for the data; K8s multi-cluster tools (Karmada, Velero, Argo, Liqo, Submariner) are only needed to move the *workload objects* (StatefulSet/PVC/PV manifests). Lunix's job is to make the PV object trivially re-creatable and to make mounting safe.
- **Fencing is the crux, and S3 conditional writes make it clean.** Single "head" object `vol/<uuid>/head` updated with `If-Match: <etag>` (CAS) holding `{epoch, owner_id, lease_expiry, manifest_ref}`; every manifest publish is a CAS on head, so a deposed writer's publishes fail with 412 no matter how stale its clock. A partitioned old node cannot corrupt S3 state; it can only lose its unflushed writes. Add (a) local self-fencing via lease expiry (stop acking FLUSH when the lease can't be renewed, then fail I/O) and (b) a kill-switch: new owner CAS-bumps epoch ("steal") after `lease_expiry + skew` or immediately with explicit `--force`. Data chunks are content-addressed and immutable, so a stale writer can at worst upload orphan chunks (GC'd later) — it cannot overwrite anything. This is precisely Constellation's epoch/CAS design (DESIGN.md §"Lease", slot CAS) transposed to one object.
- **Conditional writes are broadly available now**: S3 (`If-None-Match: *` Aug 2024; `If-Match` on PutObject/CompleteMultipartUpload Nov 2024; conditional CopyObject/Delete later), GCS (generation preconditions), Azure Blob (ETag conditions), R2 (PutObject If-Match/If-None-Match implemented per CF docs; UploadPartCopy and bucket Replication NOT implemented), Scaleway/OVH (partial; OVH Delete only `If-Match`). Make "head CAS" a hard requirement for single-writer fencing but support a degraded "lease-by-convention + operator fencing" mode for stores without it (e.g. some MinIO/Ceph versions, Backblaze B2 S3 API: UNVERIFIED).
- **Multi-backend: use "primary-ordered, per-backend manifest cursor" mirroring**, not Ceph-style bidirectional multi-master. Single writer means replication is one-directional per epoch: the writer publishes a monotonically numbered *manifest generation*; each backend has a *cursor* (highest generation fully present: all segments first, then manifest). Quorum modes: `sync(all)`, `sync(quorum k of n)`, `primary + async followers`. Adding a backend = state `Joining` (background copy of every chunk reachable from the pinned manifests + live tail replication from the moment of attach), then `InSync`; removing = `Draining` -> verify -> drop. Invariant: **a manifest/head is written to backend B only after every chunk it references exists on B**.
- **Cross-cloud copy is a data-plane job done by Lunix itself** (GET from source, PUT to destination, streamed, dedup-aware, with bounded parallelism). Server-side `CopyObject` only works inside one provider/endpoint (and cross-region within AWS); do not rely on it cross-cloud. Egress is the real cost (~$0.05-0.09/GB AWS/GCP/Azure internet egress; R2 and some others $0 egress; Hetzner/OVH/Scaleway cheap or included), so: copy *unique compressed* chunks once (dedup + zero-block elision + compression make this typically much smaller than device size), prefer sourcing from the backend with free/cheap egress, and expose a pre-flight cost estimate.
- **Cold-cache start**: mount immediately from the manifest (needs head + manifest + chunk index only), serve reads by lazy fetch with in-flight dedup, run background prefetch ordered by (a) recorded hot-set hints saved in the manifest by the previous owner, (b) filesystem-aware hints (e.g. ext4/xfs metadata regions first), (c) sequential detection/readahead. Parallel range GETs with hedged reads across backends. Writes go to local cache first (write-back) and ship asynchronously.
- **Credentials**: never in PV specs. Per-backend `Secret`s referenced by StorageClass params (`csi.storage.k8s.io/provisioner-secret-*`, `node-stage-secret-*`) or, preferably, workload identity (IRSA / EKS Pod Identity, GKE Workload Identity, Azure Workload Identity, OIDC federation to R2/others) bound to the `lunixd` DaemonSet's ServiceAccount; encryption keys are a *separate* secret class (KMS-wrapped data key stored in the volume header, unwrapped via the node's identity), so moving clouds doesn't require moving the key if the KMS is reachable cross-cloud, or re-wrapping the key per cloud.

## 1. CSI for block devices

### 1.1 CSI spec essentials (v1.x)

Three gRPC services over a unix socket:

- **Identity**: `GetPluginInfo`, `GetPluginCapabilities` (`CONTROLLER_SERVICE`, `VOLUME_ACCESSIBILITY_CONSTRAINTS`, `GROUP_CONTROLLER_SERVICE`), `Probe`.
- **Controller** (runs anywhere, one or few replicas, driven by external-provisioner / external-attacher / external-snapshotter / external-resizer sidecars): `CreateVolume`, `DeleteVolume`, `ControllerPublishVolume`/`Unpublish` (only if attach is needed), `ValidateVolumeCapabilities`, `ListVolumes`, `GetCapacity`, `ControllerGetCapabilities`, `CreateSnapshot`, `DeleteSnapshot`, `ListSnapshots`, `ControllerExpandVolume`, `ControllerGetVolume`, `ControllerModifyVolume` (VolumeAttributesClass), group snapshot RPCs (newer).
- **Node** (DaemonSet, one per node, kubelet calls it directly via registration through node-driver-registrar): `NodeStageVolume`/`NodeUnstageVolume` (once per volume per node, global "staging path"), `NodePublishVolume`/`NodeUnpublishVolume` (once per pod/container usage, target path), `NodeGetVolumeStats`, `NodeExpandVolume`, `NodeGetCapabilities` (`STAGE_UNSTAGE_VOLUME`, `GET_VOLUME_STATS`, `EXPAND_VOLUME`, `VOLUME_CONDITION`, `SINGLE_NODE_MULTI_WRITER`), `NodeGetInfo` (node id + `accessible_topology`).

**Block vs filesystem** (`VolumeCapability.access_type`): `Block` or `Mount{fs_type, mount_flags, volume_mount_group}`. With `volumeMode: Block` the target path of `NodePublishVolume` is a *file* (kubelet pre-creates it) and the driver must make the device node appear there (bind-mount `/dev/ublkbN` onto the file, or `mknod`). kubelet then uses it as `volumeDevices` (`devicePath`) in the pod. With `volumeMode: Filesystem`, the driver formats (if empty) and mounts at the staging path in `NodeStageVolume`, then bind-mounts to each publish target.

**Access modes**: `SINGLE_NODE_WRITER` (RWO), `SINGLE_NODE_SINGLE_WRITER` (RWOP, "ReadWriteOncePod", GA in K8s 1.29; requires the driver to advertise the newer capability), `SINGLE_NODE_MULTI_WRITER`, `SINGLE_NODE_READER_ONLY`, `MULTI_NODE_*`. Lunix: advertise RWO + RWOP; a read-only multi-node mode is a later feature (readers could mount a *snapshot* manifest read-only on many nodes — natural for S3-backed data, see Recommendations).

**VolumeAttachment**: only created when the CSIDriver has `attachRequired: true`. Lunix attaches nothing in a cloud-API sense (the "attach" is acquiring the S3 epoch lease, done in NodeStage), so set `attachRequired: false`. Trade-off: with `attachRequired: true` the external-attacher + `VolumeAttachment` object gives Kubernetes-level serialisation ("volume X is attached to node A, cannot attach to B" -> Multi-Attach error) *before* NodeStage; with false, kubelet will try to stage on a second node and our S3 CAS lease is the only arbiter (acceptable, and it handles cross-cluster, where K8s can't know). Recommendation: `attachRequired: false`, enforce in NodeStage via epoch lease; RWOP gives in-cluster pod-level exclusion for free.

**Topology**: `NodeGetInfo.accessible_topology` returns e.g. `{topology.lunix.io/region: ap-southeast-1}` and the controller's `CreateVolume` receives `accessibility_requirements` (preferred/requisite). For Lunix, the volume is *not* bound to a zone (S3 is regional/global), so the CSIDriver should return no topology constraint for placement, or topology only as a *hint* for which backend to use as primary / where the cache lives. Do NOT set `volumeBindingMode: WaitForFirstConsumer` as a correctness requirement; it is still useful so `CreateVolume` can learn the consumer's region and choose the nearest backend as primary.

**ExpandVolume**: `ControllerExpandVolume` (cap `EXPAND_VOLUME`) then `NodeExpandVolume` (needs node capability). For Lunix the "device" growth is a metadata change (new size in manifest; ublk size change via `UBLK_CMD_SET_PARAMS`/ `UBLK_CMD_UPDATE_SIZE` — present in recent kernels, UNVERIFIED for 7.0 exact name) and the filesystem resize (`resize2fs`/`xfs_growfs`) happens in `NodeExpandVolume`. Online expansion is supported by kubelet for both modes; in Block mode the pod sees a larger device after the capacity-change notification. Offline-only drivers set only controller expansion. Do both: the controller-side step can be a no-op that records new size in S3 if the volume is unmounted, the node-side does it when mounted. Shrink is NOT supported by Kubernetes PVC semantics (PVC size cannot decrease) — shrink must be an out-of-band Lunix CLI/CRD operation, not CSI.

**Snapshots**: external-snapshotter sidecar + `VolumeSnapshot`/`VolumeSnapshotContent`/`VolumeSnapshotClass` CRDs. `CreateSnapshot(source_volume_id, name)` -> `snapshot_id`, `size_bytes`, `creation_time`, `ready_to_use`. For Lunix a snapshot is "pin manifest generation N" (a tiny object `vol/<uuid>/snap/<id> -> manifest ref`, plus a GC root). Crash-consistent by construction; application consistency via the usual pre-freeze hooks (Velero/Kasten, or `fsfreeze` in the node plugin if the volume is mounted: optional `fsfreeze` parameter). Cheap and instant, which is a real selling point vs EBS snapshots. `VolumeSnapshot` is also the clean primitive for cross-cluster move (below).

**Clones**: `CreateVolume` with `volume_content_source` = volume or snapshot, cap `CLONE_VOLUME`. A clone is a new volume uuid whose manifest points at the same immutable chunks (CoW through dedup) — O(1). Must keep chunk GC reference-counted/mark-sweep across volumes in the same bucket. Cross-bucket clones just don't share chunks (or require copy).

**Ephemeral volumes**: generic ephemeral volumes (a PVC template in the pod spec) work with any dynamic provisioner, no extra work. CSI ephemeral inline volumes (`volumeLifecycleModes: [Persistent, Ephemeral]`) only get `NodePublishVolume` with `csi.storage.k8s.io/ephemeral: "true"` and no controller; Lunix could support this for "scratch volume backed by local FS only, no S3" (the standalone mode of the product!) — cheap to add, nice demo.

**Other relevant capabilities/flags**: `fsGroupPolicy` (`File` / `ReadWriteOnceWithFSType` default behaviour; for raw block irrelevant), `podInfoOnMount`, `requiresRepublish` (use for credentials rotation — see §5), `storageCapacity` (CSIStorageCapacity; for local cache capacity, probably false), `seLinuxMount` (Constellation settled this as false; for Lunix with real block devices `true` is possible and gives faster relabel-free mounts: UNVERIFIED benefit), `nodeAllocatableUpdatePeriodSeconds` / max volumes per node via `NodeGetInfo.max_volumes_per_node` (important: ublk device count and ring/memory budget).

### 1.2 Running a ublk-based node plugin

ublk facts relevant here (kernel `drivers/block/ublk_drv.c`, docs `Documentation/block/ublk.rst`):

- Control device `/dev/ublk-control`; per device a char device `/dev/ublkcN` (the server's command/IO channel, io_uring `URING_CMD`) and block device `/dev/ublkbN`. The container needs `/dev/ublk-control` and `/dev/ublkcN` (not `ublkb` for the daemon, but the *host* mount namespace needs `ublkbN` visible to kubelet and the pod runtime).
- Needs `CAP_SYS_ADMIN` (or the newer unprivileged mode `UBLK_F_UNPRIVILEGED_DEV` with udev rules/`ublk_drv.ublks_max`; for containers privileged is simpler). `ublk_drv` module must be loaded on the host (DaemonSet initContainer `modprobe ublk_drv`, or document as node prerequisite; `modprobe` from a container requires `/lib/modules` mount).
- Modes: `UBLK_F_USER_RECOVERY` (device survives server death; I/O quiesced; `UBLK_U_CMD_START_USER_RECOVERY` / `END_USER_RECOVERY`), `UBLK_F_USER_RECOVERY_REISSUE` (in-flight I/O re-issued to new server after recovery rather than failed), `UBLK_F_USER_RECOVERY_FAIL_IO` (fail I/O immediately while server is down), `UBLK_F_USER_COPY`, `UBLK_F_ZONED`, `UBLK_F_UNPRIVILEGED_DEV`, `UBLK_F_NEED_GET_DATA`, `UBLK_F_AUTO_BUF_REG` (newer), batch commands (newer). Exact availability on Linux 7.0 must be checked with `UBLK_U_CMD_GET_FEATURES`.
- Without recovery mode, when the server process exits the kernel *deletes* the ublk device (after `UBLK_CMD_STOP_DEV` semantics on fd close/ daemon exit) and any mounted filesystem sees EIO/disappears. That is the nbd-like failure mode we must not ship.

**Process model** (recommended):

```
node
|- lunixd DaemonSet pod (privileged, hostPID not needed)         <- DATA PLANE, long-lived
|   - owns /dev/ublk-control, /dev/ublkcN, io_uring, local cache dir (hostPath or local PV)
|   - owns S3 clients, lease renewal, background sync
|   - state dir /var/lib/lunix (hostPath): per-volume config, ublk dev id <-> volume uuid, lock
|   - control socket /var/lib/lunix/run/lunixd.sock (gRPC or Constellation-style framed control proto)
|- lunix-csi-node pod (plugin, node-driver-registrar, liveness)   <- THIN, can restart freely
|   - Node service on /var/lib/kubelet/plugins/lunix.csi/csi.sock
|   - NodeStage: ask lunixd "attach volume uuid with backends [..] and epoch-lease" -> returns /dev/ublkbN
|   - mount/mkfs, bind mounts (needs Bidirectional mount propagation on /var/lib/kubelet)
|- lunix-csi-controller Deployment (provisioner, resizer, snapshotter sidecars + controller service)
```

Notes:
- **Mount propagation**: the node plugin container mounts `/var/lib/kubelet` (hostPath, `mountPropagation: Bidirectional`) so mounts created inside the container (staging path, bind mounts into pod target paths) are visible to kubelet/host and vice versa. Needs `privileged: true`. `lunixd` needs `/dev` (hostPath `/dev` or specific devices) and `/sys` read; it does NOT need Bidirectional propagation if it only creates block devices (devtmpfs entries appear in host `/dev` since the char/block nodes are created by kernel/udev in the host's devtmpfs; inside a container with hostPath `/dev` you see them; inside a container with its own `/dev`, you need to mknod or bind hostPath `/dev`). Practical: mount hostPath `/dev` into both pods.
- **Why the data daemon must be separate from the plugin**: plugin upgrades (frequent) must not interrupt I/O. Real-world precedent: Longhorn runs "engine"/"instance-manager" pods separate from the CSI plugin and keeps iSCSI/NVMe frontends alive; its `instance-manager` pods are only drained on node upgrade, never on CSI plugin upgrade. Ceph-CSI's rbd-nbd mounter has the opposite design (daemon in plugin pod) and has repeated "I/O error after plugin restart" issues; Rook's docs: nbd/fuse not recommended in production. Constellation made the same decision: engine pods (`crates/csi/src/engine_pods.rs`) separate from plugin, with FUSE session handover (`node/handoff.rs`) for zero-loss engine upgrades.
- **Surviving `lunixd` upgrade/crash**: use `UBLK_F_USER_RECOVERY` (+ `REISSUE` for writes that must not be lost — but beware duplicated writes are idempotent at block level, so REISSUE is safe for a block device) and have the new `lunixd` re-open `/dev/ublkcN`, replay local journal, check epoch still ours, `END_USER_RECOVERY`. The state dir lock (flock) prevents two daemons from serving concurrently (same trick as Constellation). For planned upgrades: new daemon starts as standby, old daemon quiesces/flushes local journal, old exits, new takes over (`ublk` quiesce window = pause the pod sees; target < 1s, bounded by io_uring teardown). Pods see a latency spike, not an error — K0-style spike should measure: the stall tolerance of ext4/xfs and typical DBs (they tolerate multi-second stalls; hung-task watchdog default 120s is the hard limit).
- **Node reboot**: ublk devices vanish; kubelet re-runs NodeStage/Publish on pod restart; `lunixd` start must be able to re-attach all volumes lazily (state dir records them) or on demand on NodeStage.
- **Orphan cleanup**: `NodeUnstageVolume` must tolerate "device already gone", must be idempotent (CSI requires), and a GC loop in lunixd removes ublk devices without a staged volume (cf. `crates/csi/src/node/gc.rs` in Constellation).
- **Device naming**: ublk ids are dynamic; give `NodePublish` a stable path via `/dev/disk/by-id/lunix-<volid>` symlink (udev rule inside container isn't visible to host; create the symlink from the node plugin on the host-mounted `/dev`) or just bind-mount the device to the pod target file (the standard for raw block; pods never see the host name).
- **Eviction / drain**: on `NodeUnstageVolume` lunixd must flush local cache dirty data to S3 (in async mode, within bounded time) *before* releasing the lease; if S3 unreachable, keep the dirty cache on local disk and release lease only if user allowed "lose tail" — otherwise refuse unstage and keep retrying (kubelet will retry; the volume stays "stuck" which is correct: it protects data). Need a force-unstage with `lunixctl volume abandon`.
- **Stats & health**: `NodeGetVolumeStats` for block: `total_bytes` only (Block usage unknown); for filesystem `statfs`. `VOLUME_CONDITION` abnormal = lease lost, S3 unreachable, cache full, backend degraded, which kubelet surfaces as events (`NodeGetVolumeStats.volume_condition`).

### 1.3 What real drivers do (quick survey)

- **Longhorn**: controller (manager) + per-volume "engine" process in an instance-manager pod, replicas on other nodes, frontend is iSCSI (tgt) or NVMe-oF/ublk (v1.7+ "ublk frontend" exists, UNVERIFIED current maturity). CSI plugin does attach (`ControllerPublish` -> manager attaches engine to node), then `NodeStage` logs in iSCSI and mkfs/mount. Lessons: engine lives outside plugin pod; "attach/detach" state machine in CRDs (`volumes.longhorn.io`, `engines`, `replicas`) is a heavy but robust approach; backups go to S3/NFS with a block-level incremental scheme (2MB blocks, dedup by hash) — closest conceptual relative, including DR volumes in another cluster pointing at the same backup target and "restore" from backup (this is Longhorn's version of pod mobility: backup + restore, minutes to hours, not live).
- **OpenEBS Mayastor (Replicated PV Mayastor)**: SPDK-based io-engine in a DaemonSet pod, NVMe-oF exposed; CSI node plugin connects with `nvme connect` (kernel initiator) — data daemon again separate. Uses `DiskPool` CRDs. Local cache-ish tiers: none. ublk not used for the frontend (UNVERIFIED; it exposes via NVMe-oF).
- **TopoLVM**: local LVM VGs; topology-aware via scheduler extender / CSIStorageCapacity; `lvmd` runs as a separate DaemonSet process (gRPC over unix socket) and the CSI node plugin talks to it — same split we want. Pure kernel data path (dm), so no data-daemon-restart problem.
- **csi-driver-host-path**: reference driver; supports snapshots/clones/block via loop device (`losetup`) and ephemeral inline volumes; shows the `volumeMode: Block` NodePublish pattern (bind-mount `/dev/loopN` onto the target file) and the `fsfreeze` hook. Good test target for Lunix's sanity (csi-sanity).
- **SPDK-CSI** (spdk/spdk-csi): node plugin connects SPDK NVMe-oF/iSCSI volumes and mounts; controller talks to SPDK JSON-RPC (`xPU` model).
- **Ceph CSI rbd**: modes `krbd` (kernel; robust) vs `rbd-nbd` (userspace; supports newer features/ QoS) vs `rbd-nbd` with `try-others`. rbd-nbd runs in `csi-rbdplugin` pod; known problems: plugin restart kills nbd daemon (ceph-csi #5068: SIGTERM does not reach rbd-nbd; watcher not released; ~30 s I/O interruption), mount goes read-only on nbd death (Ceph tracker #51820), volumes stuck on node reboot (Rook issue #11575 family). Ceph-CSI later added "detach on restart / re-attach `rbd-nbd` via `--try-netlink` + `reattach` logic" (UNVERIFIED which versions). Direct lesson: **userspace block daemons in the plugin container are a known production hazard; keep them separate and give the kernel device a recovery mode.**

Conclusion: the established pattern is `{controller, node plugin (thin), data-plane daemon (separate pod)}`; Lunix should copy it, plus ublk user-recovery as a safety net that the nbd world doesn't have.

### 1.4 Sharing with Constellation's CSI crate

Local code reviewed: `/home/bra/cvs/replicix/constellation/crates/csi/` (≈9.6k lines: `controller.rs` 1243, `engine_pods.rs` 4259, `credentials.rs` 720, `params.rs` 656, `volume_id.rs` 427, `purge.rs` 1405, `node/{mounter,handoff,rollout,gc,state,engines}.rs`, `identity.rs`, `control_client.rs`; deps: tonic 0.14 + prost 0.14, `kube` 4.2, `k8s-openapi` 0.28 v1_36, rustls/ring, zeroize). Plan: `docs/plans/v1/done/37-kubernetes-csi.md`; user guide `docs/how-to-guides/kubernetes-csi.md`.

Directly reusable (as a shared crate or copy-then-diverge; per memory notes Constellation is modifiable, so factoring is allowed):
- **Proto + tonic scaffolding** (`build.rs`, `proto/`, `identity.rs`, `main.rs` socket serving, node-registrar glue, graceful shutdown): CSI v1 gRPC is identical. Candidate: extract `csi-core` crate (Identity service, socket serving, capability helpers, parameter parsing helpers from `params.rs`, request-validation helpers).
- **`credentials.rs` design**: secrets only via the control socket, held in an in-memory store, wiped (zeroize), core dumps off, `refreshing` mode via Secret watch with `resourceNames`-scoped RBAC and backoff re-list (1 s -> 30 s). Directly applicable to per-backend S3 keys + encryption passphrase for Lunix, including the "plugin restart forgets secret, kubelet's next NodeStage re-supplies it" semantics.
- **`volume_id.rs` codec idea**: versioned, self-describing, no registry/CRD needed (`v1/<layout>/...`); Lunix equivalent `v1/<volume-uuid>` or `v1/<backend-set-hash>/<uuid>`; plus an unversioned "static" shape for hand-written PVs.
- **Engine-pod lifecycle (`engine_pods.rs`)**: the controller creates/owns engine pods with the `kube` crate; Lunix probably does NOT need per-volume pods (ublk in one DaemonSet process is lighter), so most of the 4k lines is Constellation-specific. Reuse only ideas (pod ownership, readiness).
- **Handoff protocol (`node/handoff.rs`)**: stepwise quiesce -> drain -> transfer (SCM_RIGHTS fd passing over socketpair) -> seal -> commit with rollback before commit. For ublk the equivalent is simpler thanks to kernel recovery: new daemon opens `/dev/ublkcN` itself; only the "state dir lock + quiesce old + replay journal" parts are needed. Reuse the *rollout controller* patterns (`node/rollout.rs`) for upgrading the DaemonSet safely.
- **`control_client.rs` / constellation-control**: Lunix needs its own control protocol; sharing the framed typed-method-table crate (`constellation-control`) might be possible but couples release cycles. UNVERIFIED how generic it is; recommend a small shared `replicix-csi-common` crate for gRPC scaffolding, credentials and volume-id helpers, and leave control protocols separate.
- **`purge.rs` (1405 lines)**: GC/trash of deleted volumes — Lunix's `DeleteVolume` is "drop head + mark chunks unreferenced", different, but the retention/trash-window idea (`DeleteVolume` moves to `.trash`, purge later) is worth copying: deleting a PVC in K8s is irreversible and users make mistakes.

## 2. Pod mobility across clusters and clouds

### 2.1 What moving looks like

Kubernetes has no native cross-cluster PV movement. A stateful pod move = (1) quiesce the workload in the source, (2) make the volume's durable state available to the destination, (3) recreate PV/PVC and workload in the destination cluster, (4) start with a cold cache. Today people use backup/restore (Velero + CSI snapshots + data mover/Kopia/Restic; Kasten K10 export; Longhorn DR volumes, Ceph RBD mirroring), costing minutes to hours proportional to data size, because the data is copied before start. Lunix's advantage: **the data already lives in an object store that any cluster can reach**, so "restore" is O(metadata) and the copy is lazy.

Two levels:
1. **Same bucket(s), different cluster/region** (cross-region read latency to the original bucket): just mount. Works day 1 with a static PV. Penalty: every cache miss crosses regions (100-300 ms RTT US<->Asia, plus egress fees).
2. **Migrate the data to a bucket local to the destination** (add backend, sync, drop old backend): the multi-backend feature (§3). Can be done *before* the move (pre-warm: add destination bucket while still running at the source; this gives a near-zero-downtime move), or *after* (start at destination reading from the far bucket, migrate in background).

### 2.2 Fencing the old writer

Threat model: the old node is partitioned (still running, still holding local dirty cache and a ublk device) while the new node mounts. Requirements: (R1) the old node can never publish state to S3 that the new node could consider valid after the handover point; (R2) ideally the old node stops acking writes to its pod soon after losing the lease (otherwise the old pod diverges in ways nobody sees; it's a fenced-off ghost, acceptable if its output cannot reach clients, but self-fencing is better); (R3) a handover has a clear data-loss boundary.

Design (head object CAS lease, Constellation §Lease analogue):

```
vol/<uuid>/head       (small JSON/CBOR, ~1 KB, updated via If-Match on ETag)
  { format_version, volume_uuid, size, epoch: u64, owner: {node_id, cluster, region},
    lease_expiry_unix_ms, manifest: {gen: u64, key: "m/000000123.lxm", sha256}, flags, backends: [...] }
vol/<uuid>/m/<gen>.lxm         immutable manifest generations (chunk map delta/checkpoint)
vol/<uuid>/chunks/<hh>/<hash>  immutable content-addressed chunks (or shared bucket-wide `chunks/`)
```

Protocol:
- **Acquire** (NodeStage): GET head (ETag e). If `owner == none` or lease expired by `lease_expiry + max_skew` (or `released: true`): PUT head with `epoch+1`, new owner, new expiry, `If-Match: e`. 412 => someone else won, retry/fail. For a brand-new volume, `If-None-Match: *`.
- **Renew**: every `lease/3` CAS-update head with new expiry (same epoch). Renewal piggybacks on manifest publishes (a publish is also a renewal), so steady-state cost is ~1 PUT per sync interval; use long leases (e.g. 60 s TTL, renew 20 s) since PUT cost is negligible ($0.005/1000).
- **Publish manifest** gen g+1: write chunks (immutable, unconditional) -> write `m/g+1` (`If-None-Match: *`) -> CAS head `{epoch same, manifest g+1}` with If-Match. If head's epoch != ours (CAS fails or reading shows higher epoch) => **fenced**: stop everything.
- **Self-fence (R2)**: if lease renewal fails for `lease_ttl - safety`, the daemon stops acking FLUSH and then fails or blocks I/O (policy: `on_lease_loss = freeze|eio`). Since the guest FS might have been fine-grained, `freeze` (quiesce I/O, ublk stays) lets an operator decide; `eio` makes DBs crash and restart elsewhere. A monotonic clock (CLOCK_MONOTONIC) for local lease deadlines avoids wall-clock jumps; server timestamps are not needed because expiry is compared with *the new owner's* clock + skew margin; to be safe the new owner waits `lease_ttl + max_skew` after the last observed head change before stealing (like Chubby/ZK grace). Explicit `release` on clean unstage skips the wait.
- **Kill-switch / forced steal (operator)**: `lunixctl volume steal <vol> --reason` CAS-bumps epoch immediately, ignoring the lease. Old owner's next publish/renew fails with 412 and it self-fences. Data lost = old owner's unpublished writes; make it explicit in output (last manifest gen, wall-clock). Also an out-of-band kill: the new owner (or a controller) can set `fence_token` that the old node polls (every few seconds; useful if S3 reachable but K8s control plane is not).
- **Storage-side fencing** goes further: because chunks are immutable and unique by hash, there is nothing the old node can overwrite; head and manifests are the only mutable/ordered objects, and both are CAS-protected. The only residual hazard is a *delayed* in-flight PUT of head from the old node arriving after the new owner's CAS: it still carries `If-Match` with an old ETag => 412. Good: S3 gives linearizable conditional writes (strong consistency since Dec 2020).
- **Stores without conditional writes**: degrade to "operator-fenced" mode (lease object + sequential epochs in key names `head/<epoch>.json` written with plain PUT and relying on listing; unsafe under partitions). Detect at backend-add time with a probe (`PUT If-None-Match: *` twice; expect 412), refuse multi-node mount without the capability, or require `--i-know`.

Multi-backend interplay: head is replicated per backend; one backend is the *authority* for head CAS (the "lease backend", usually the lowest-latency/most reliable) and the others receive head copies as part of normal manifest publishing. Fencing authority must be a single object; do not try to CAS across clouds (no cross-cloud atomicity). If the authority backend dies, an operator promotes another (`lunixctl backend set-authority`), which is itself a CAS-ed epoch bump with a "lease fence" period. Alternative (quorum CAS across 3 backends, Paxos-on-object-stores à la `Hocuspocus`/"Skyplane"?): possible via per-backend `If-None-Match` slot objects (`claim/<epoch>` created on majority of backends = ballot win), analogous to Constellation's slot CAS. Keep as v2: complexity high, single authority is fine.

Partitioned old node when the new node mounts "gracefully": the control plane (K8s) tells the old node to unstage; if the old node is dead/partitioned, K8s never confirms detach (the infamous 6-min `ReadWriteOnce` Multi-Attach delay for EBS: `attachRequired: true` + force detach timeout; non-graceful node shutdown taint `node.kubernetes.io/out-of-service` allows forced pod deletion and volume detach — stable since 1.28). For Lunix, since the lease is in S3, the new node just waits lease TTL and steals; no K8s-level detach is needed, so failover is bounded by `lease_ttl` (e.g. 30-60 s) rather than K8s timeouts.

### 2.3 Cold cache and hydration

Prior art: EBS snapshot restore (fast snapshot restore vs lazy-loading from S3 with first-touch penalty, "initialize" via `dd`/fio to hydrate), Azure/GCP snapshot-restored disks similar, Firecracker/Lambda container image lazy loading (SOCI, eStargz), Nydus/DragonflyDB image acceleration, JuiceFS (block cache + `warmup`), Longhorn restore (full copy first), VMware vSAN.

Strategy for Lunix:
1. **Mount in O(manifest)**: fetch head + latest manifest (+ chunk index pages as needed; keep the index in a B-tree/radix-structured page set so only touched pages load — avoid a monolithic multi-GB chunk map). A 1 TiB volume with 64 KiB average chunk = 16 M entries ≈ 16M×(32 B hash + 8 B meta) ≈ 640 MB flat; use paged index (e.g. 4 KiB-page B+tree or sorted runs with fence pointers) and cache the top levels.
2. **Demand reads**: LBA -> chunk(s) -> local cache lookup -> miss => ranged GET; coalesce concurrent misses for the same chunk (single-flight); issue speculative read-ahead for sequential patterns; use hedged requests to a second backend if p95 deadline exceeded (§3.4).
3. **Background prefetch with priorities**: (a) hot-set list stored in manifests by previous owner (e.g. top-N chunk ids by recent access, sampled via a CLOCK/LRU-K counter; storing a Bloom/roaring bitmap of "hot LBAs" ~ 100s KB per sync, extremely cheap); (b) filesystem metadata (superblock, group descriptors, journal, inode tables for ext4; AG headers for xfs) heuristically located by probing the guest FS superblock in NodeStage when Lunix formatted it (it knows fs_type) — "fs-aware prefetch"; (c) the rest by LBA order or by recency stored in the manifest. Throttle prefetch to X% of bandwidth and yield to demand reads (separate io_uring submission queues / priority classes).
4. **Write-back during hydration is fine**: writes go to the local cache and are shipped; reads of unfetched chunks still need a GET (unless fully overwritten: partial-chunk writes require read-modify-write of the chunk => a miss on write too; mitigate by sub-chunk dirty-extent journaling so a 4K write doesn't need to fetch 64K+ chunk: store as delta/overlay chunk — design question for the core chunk store, noted for other research notes).
5. **Make cost visible**: `lunixctl volume warm --policy hot|all|none --bandwidth 200MB/s --budget-usd N` and metrics `lunix_cache_hit_ratio`, `lunix_fetch_bytes_total{backend}`.
6. **Pre-warm for planned moves**: destination node can attach as a *read-only follower* of a running volume (reading manifests as they appear and prefetching the hot set) before the cutover. Because manifests are immutable and chunks content-addressed, a follower is trivial: it tails `head` (poll or S3 event notification) and fetches new chunks. Cutover then is: source flushes final manifest, releases lease; destination acquires and is already warm. This gives sub-second pause for the move itself (limited by final flush + K8s pod restart, which is the real cost: seconds).

### 2.4 Multi-cluster tooling and volume portability

- **Velero** (backup/restore of K8s objects + CSI snapshots; "data mover" via Kopia to object store): for Lunix the PV data need not be copied; Velero's CSI plugin would create a `VolumeSnapshot` (cheap pin), and on restore in the other cluster create a PVC from `dataSource` snapshot (needs `VolumeSnapshotContent` pre-provisioned with `snapshotHandle` = Lunix snapshot id and the driver able to resolve it — works if the snapshot id is self-contained: `v1/<volume-uuid>/<snapshot-id>` + backends from the StorageClass). Set Velero to not use the data mover for Lunix classes (`snapshotMoveData: false`). Caveat: Velero restores need the destination cluster's snapshot class to point at accessible backends; the Lunix snapshot handle must be resolvable without a registry — hence the self-describing ID.
- **Kasten K10**: similar; "export and import" of restore points between clusters; CSI snapshot based.
- **Karmada / Open Cluster Management / Fleet / Argo CD ApplicationSets**: propagate Deployments/StatefulSets/PVCs by policy; they do not move data. Use Karmada `PropagationPolicy` with cluster affinity switched from cluster A to B + `ResourceInterpreterCustomization`; PVC/PV propagate as plain objects, so a **static PV** with `volumeHandle` + `nodeStageSecretRef` can be propagated verbatim. Karmada's "failover" feature (cluster taint -> reschedule) combined with Lunix lease timeout gives automatic DR with bounded data loss.
- **Liqo**: virtual-node peering; pods scheduled to a remote cluster via a virtual kubelet; Liqo ≥ 0.9 has "storage" support by *virtual storage class* that re-creates PVC in the remote cluster and **supports moving PVC between clusters via a `liqo-storage` migration using a Rsync-like job** (UNVERIFIED details). With Lunix, the remote cluster can just mount the same volume id — a very good fit because pods "move" without data copy.
- **Admiralty**: multi-cluster scheduler with proxy pods; doesn't handle storage; same static PV approach.
- **Submariner / Skupper / Cilium ClusterMesh**: network connectivity for services (cross-cluster service discovery) — relevant to keep app endpoints stable when the pod moves; orthogonal to storage.
- **Portable volume ID**: `volumeHandle: lunix1:<volume-uuid>` + `volumeAttributes: {backends: "s3://us-east-bucket/pfx,s3://ap-bucket/pfx", ...}` or reference a cluster-scoped `LunixBackendSet` CRD name (`backendSet: prod-global`) that each cluster defines locally (so URLs/credentials/identity differ per cluster, but the volume uuid is global). The `StorageClass`/`LunixBackendSet` resolves "where are the buckets" and "how do I auth". This separation (identity = uuid; location = per-cluster config; truth = S3 head) is what makes IDs portable without a cross-cluster controller. A cross-cluster controller is only needed for *orchestrating* (add backend, wait in-sync, flip workload, remove backend); implement as optional `LunixMigration` CRD reconciled by the controller in either cluster, or driven by a CLI/Argo workflow (v1: CLI + documented runbook).
- **Cluster-identity in head**: record `owner.cluster_id` (from a `lunix-cluster-id` ConfigMap UUID) so `lunixctl volume status` shows who holds it; prevents confusion ("which cluster owns this volume?") and enables guardrails (refuse to steal from a *live-looking* owner without `--force`).

## 3. Multi-backend mirroring

### 3.1 Data model for N backends

State in each backend (a backend = {endpoint, bucket, prefix, credentials ref, capability flags: cond-writes, server-side-copy, versioning}):

```
chunks/<aa>/<hash>.lxc          immutable; may be a bucket-shared namespace across volumes (dedup)
vol/<uuid>/m/<gen>.lxm          immutable manifests, each lists chunks it references (or the delta)
vol/<uuid>/head                 mutable CAS object; per-backend copy; carries `cursor` info
vol/<uuid>/backends.json        (inside head) list of backends with states
```

Per-backend state machine (stored in head, replicated to all in-sync backends):

`Joining(copy_cursor)` -> `CatchingUp` -> `InSync` -> (`Degraded` <-> `InSync`) -> `Draining` -> `Removed`

- **Replication unit**: manifest generation `g`. Backend B's `cursor_B` = highest `g` such that *all chunks referenced by manifests ≤ g (that are still live) exist on B* and `m/g` and `head` (with `manifest=g`) are written on B. Writer flow per sync point: (1) ship new chunks to all targeted backends in parallel; (2) once chunk PUTs are acknowledged on backend B, write `m/g` to B; (3) CAS head on authority; (4) PUT head on other backends (plain PUT or If-Match on their own ETag; the head on non-authority backends is an advisory copy used for recovery, it never arbitrates leases).
- **Invariant I1**: `m/g` and `head.manifest=g` appear on B only after all chunks reachable from `m/g` exist on B. (Chunk-first, manifest-last; S3 is read-after-write consistent, so a reader that sees `m/g` can GET its chunks.) For chunks referenced by old manifests but not new ones, I1 applies to "referenced by `m/g`".
- **Invariant I2**: epoch monotonic; a manifest publish carries the writer's epoch; recovery picks max(epoch, gen) among reachable backends' heads, then validates I1 by sampling/full check ("scrub").
- **Invariant I3**: chunk objects are write-once; same hash => same content (verify hash on read; mismatch => treat backend copy corrupt, repair from another mirror).

### 3.2 Replication modes

| Mode | Write ack to guest FLUSH | RPO | Latency | Notes |
|---|---|---|---|---|
| `sync-all` | all backends have chunks+manifest | 0 | max over backends (cross-ocean 200+ ms) | Rarely wanted across oceans |
| `sync-quorum(k of n)` | k backends | 0 for k-1 failures | k-th fastest | Remaining backends lag as async followers; need a per-backend cursor and catch-up queue |
| `primary + async followers` | primary only (or local cache in async mode) | followers lag (replication lag reported; `lag_gens`, `lag_seconds`, `lag_bytes`) | primary's latency | Default; best fit for "bounded loss window" product vision |
| `local + async to all` | local FS (write-back) | window = sync interval | lowest | Combined with the product's async mode |

A follower is just "another destination for the same chunk/manifest stream" with its own queue, retry/backoff, and a bounded backlog; if the backlog exceeds a limit the follower is marked `Degraded` and falls back to "copy missing chunks by diff" (set difference between live chunk set and what it has, rather than replaying a log). Failure of an async follower never blocks the primary (unless policy `require_followers_within = N gens`, in which case apply backpressure to the guest — an explicit "RPO guarantee" knob).

Chunk shipping order optimisation: since chunks are content-addressed, a follower may already have the chunk (dedup) -> HEAD (or batch existence check via bloom + sync of listing) before PUT. Keep a per-backend *known-present chunk set* (local persistent bloom/cuckoo filter or a local sqlite/LMDB set), rebuilt from LIST on demand; avoid per-chunk HEAD where possible (0.0004$/1000 HEAD, but latency).

### 3.3 Adding a backend live

1. **Probe** the new backend: credentials, `PUT If-None-Match`, multipart, max object size, CopyObject availability (same provider?), list permissions, round-trip latency and throughput (measured), clock skew (using `Date` header).
2. **Register** it in head as `Joining`, with `join_gen = g0` (current manifest). From now on, all *new* chunks and manifests are also queued for B (tail replication begins immediately; this is the "catch-up of the tail").
3. **Backfill**: compute the set `S` of chunks referenced by pinned manifests (current head, all snapshots, retention-window manifests; optionally only head + named snapshots, by policy), walk the manifest(s) -> list of chunk ids -> for each chunk not yet on B: GET from the best source (nearest/cheapest-egress backend; local cache if present) and PUT to B with bounded concurrency (adaptive: AIMD on 503/SlowDown, `Retry-After`), throttled by a configurable bandwidth cap so production traffic isn't harmed. Same-provider case: use server-side `CopyObject` (up to 5 GiB single op; larger via multipart `UploadPartCopy`) — no egress through the node; cross-provider: stream through node (node pays ingress from source + egress to destination: **egress costs are charged to the source cloud only**, but the node's own cloud may also charge if the node is in a third place; place the migration worker in the *source* region or in the destination region to avoid double hops). Copy order: reverse recency/hotness first, so that if the user cancels early the most valuable chunks are present; the destination can also begin serving *reads* for chunks already copied (read-through).
4. **Cut to `CatchingUp`**: when backfill queue is empty, tail queue (new chunks since `g0`) is drained; B's cursor = current gen minus small lag.
5. **Promote `InSync`**: B's cursor within `epsilon` gens (or equal at a quiescent point). Writer atomically (CAS head) flips B to `InSync`; from then it participates in the sync quorum/`primary` selection.
6. **Verification (optional but recommended before removing old)**: scrub = for each manifest-pinned chunk, HEAD/LIST compare size (cheap) or GET+hash (expensive; sample percentage). Manifest `m/g` listing contains per-chunk size/hash enabling LIST-based verification (LIST 1000 keys/request is cheap).

The backfill is idempotent and restartable (state = per-backend progress cursor over the sorted chunk-id set + bloom of done), survives lunixd restarts and even pod moves (the cursor is in head under `backends[B].backfill`, flushed periodically), which also means the *new owner after a pod move continues the backfill*. Multiple nodes can't run it simultaneously unless sharded by chunk-id range with lease-per-shard (v2; useful to saturate links: a single node's NIC is often the limit, e.g. 10-25 Gbps vs. many TB).

Large volumes: copying 10 TB at 1 Gbps = ~22 h; at 10 Gbps = 2.2 h. A controller-side "migration worker" Job (a pod running the same copy library as lunixd, not attached to any ublk) can run on a separate beefy node with more bandwidth; both reuse a `lunix-sync` crate. Make the copier a standalone binary (`lunix-sync`) usable as a Job; just as `rclone` or `restic copy`.

### 3.4 Reading from the best backend

- Maintain per-backend rolling latency/throughput/error stats (EWMA + p95 per request size class) and a cost weight (egress price). Read policy: choose lowest `expected_latency*w_lat + cost*w_cost` among backends where `cursor >= needed_gen` / chunk presence is known (all InSync backends have all chunks by I1; Joining backends only those copied).
- **Hedged requests**: if the first GET isn't complete by the backend's p95 (or fixed 2x median), launch the same GET to the second-best backend and take the first response (Dean & Barroso "Tail at Scale"); cap hedge rate (e.g. ≤5% of requests) and never hedge across a costly egress path unless the user set `hedge=cross_cloud`. Cancel the loser (drop connection; note: you still pay for bytes already sent).
- Verify hash on every fetch (chunk id = hash of plaintext or ciphertext; cheap vs network).
- Write path: parallel PUTs to all sync targets; ack on policy; track and surface slow backend (don't let one slow backend stall `sync-all`: policy `sync-quorum` tolerates).
- Failure handling: error rate threshold -> `Degraded`: stop reading from it, continue background retries; re-probe with backoff; when it recovers do a diff-catch-up.

### 3.5 Removing a backend gracefully

1. Mark `Draining` in head (CAS). New writes no longer target it *after* a barrier gen `g_r`.
2. Ensure remaining InSync backends satisfy the replication policy (e.g. at least 1; refuse to remove the last InSync backend, or the authority without `set-authority`).
3. Optionally verify (scrub) the remaining backends.
4. Move head authority if needed.
5. Remove from head (`Removed`), stop all I/O to it. Leave the old data intact by default (user decides); offer `lunixctl backend purge-data` which deletes `vol/<uuid>/` and exclusively-referenced chunks (for shared dedup namespaces: refcount/mark-sweep GC needed; unsafe to delete chunks other volumes reference — hence a per-volume prefix for chunks is simpler for purge, at the cost of cross-volume dedup; trade-off to settle in the storage-layout note).
6. Hard-failure removal (backend gone): `lunixctl backend remove --force` skips draining.

### 3.6 Cross-cloud costs and limits (verify before publishing)

List prices (approximate; check current):
- **Egress to internet per GB**: AWS S3 ~$0.09 (first 10 TB; first 100 GB/month free), GCS ~$0.08-0.12 (Premium tier, varies by destination), Azure ~$0.087 (first 100 GB free), Backblaze B2 $0.01 (free up to 3x stored), Cloudflare R2 $0 egress (storage ~$0.015/GB-mo), Wasabi $0 (fair-use), Hetzner Object Storage ~€1 per TB beyond included traffic, OVH object storage: egress ~€0.01/GB (Standard) or free in some regions (UNVERIFIED), Scaleway ~€0.01/GB after 75 GB free (UNVERIFIED). Inter-region AWS: $0.02/GB (from page: us-west-2 -> us-east-1 $0.02/GB confirmed in AWS pricing example); inter-region GCP/Azure ~$0.02-0.08.
- **Requests**: S3 Standard PUT $0.005/1000, GET $0.0004/1000 (GET confirmed from AWS pricing page; PUT from memory). With 1 MiB average chunk and 10 TB: ~10 M PUTs = $50 request cost, trivial vs egress ($900 at $0.09/GB for 10 TB raw). With 64 KiB chunks, 160 M PUTs = $800: **chunk size matters enormously for request cost and for per-object overhead** (pack small chunks into larger "pack" objects with an index, like restic/borg/kopia packs, or Constellation segments — the brief's mention of "segments" indicates Lunix may already pack chunks into segments; copy granularity then = segment, with range-GET to extract live chunks when segments are sparse).
- **Server-side copy**: `CopyObject` only within the same endpoint (same provider; AWS supports cross-region, cross-account within AWS; GCS `rewrite` within GCS; Azure `Copy Blob From URL` can copy from any URL readable by SAS incl. other clouds -- Azure's "Put Blob From URL" and `Copy Blob` with source URL **can** pull from arbitrary HTTP(S) sources, which makes it the one exception that does data movement server-to-server cross cloud; the data is still billed as egress from the source). R2 documents `UploadPartCopy` as not implemented (CF docs). Max single-op sizes: S3 CopyObject 5 GiB; multipart parts 5 MiB-5 GiB, ≤10,000 parts, 5 TiB object.
- **Compression/dedup savings**: copy cost is computed on *unique stored (compressed, deduped, zero-elided)* bytes, e.g. a 2 TiB volume with 40% used and 2x compression ≈ 400 GB to move ≈ $36 at $0.09/GB.
- **Bandwidth**: single S3 GET stream ~50-100 MB/s; use parallel ranged GETs and many concurrent PUTs (S3 prefixes scale to 3,500 PUT / 5,500 GET per second per prefix; spread chunk keys by hash prefix).
- **Inter-vendor consistency quirks**: multipart ETag differs from MD5; some stores' ETag semantic for CAS differ (still opaque, fine). Storage class / minimum-duration charges (IA/Glacier) irrelevant; S3 Express One Zone has no cross-region and lower consistency semantics: UNVERIFIED for conditional writes (supports `If-None-Match`? yes announced for directory buckets? UNVERIFIED).

### 3.7 Prior art and what to take from it

- **rclone sync/copy**: stateless diff by name/size/mtime/hash, parallel transfers, `--checkers`, `--bwlimit`, server-side copy flag, retries. Take: restartable stateless "make dst a superset of src" for content-addressed chunks is trivial since the identity is the hash (no mtime heuristics, no deletes of live data).
- **restic copy / kopia repository sync**: copy snapshots between repos, copying only blobs missing at destination; verifies repo structure (packs + indexes) -> directly analogous to "copy chunks referenced by pinned manifests". Take: index-first design, `check --read-data-subset` for scrub.
- **Ceph RGW multisite**: bidirectional, async, per-zone log of mutations (datalog/bilog), full sync then incremental sync with markers; multi-master leads to conflict resolution (last-writer-wins per object) -> we avoid it by single-writer. Take: *full sync + incremental sync marker* model and sync status reporting (`radosgw-admin sync status`).
- **MinIO site replication / bucket replication**: replicates objects, versions, deletes based on events, metadata incl. IAM; requires versioning; lag metrics; "existing object replication" via batch jobs `mc replicate resync`. Take: resync = backfill job; per-target lag metrics; Lunix does it at the application level so it works across vendors.
- **S3 CRR/SRR**: asynchronous, needs versioning, 15-minute RTC SLA with Replication Time Control (paid); NOT cross-vendor. Could be used as an *accelerator* when both backends are AWS (let AWS replicate chunks, Lunix just checks completeness before publishing the manifest on the target — but I1 requires verifying presence, so Lunix still must HEAD each chunk, so little gain; skip in v1).
- **JuiceFS `sync` / dual-bucket**: `juicefs sync` is rclone-like; JuiceFS itself has no native multi-bucket mirror but supports "multiple object storage in one volume" in the enterprise edition (UNVERIFIED). Take: local cache semantics and `warmup`.
- **Litestream / LiteFS**: WAL shipping to S3 with generations/snapshots; replicas (multiple replica targets in config, each independent), LiteFS lease via Consul for primary election. Take: Litestream's model of *independent replicas, each with its own position*, and generation/snapshot retention; LiteFS lease = lease-based single-writer (we do it in S3 CAS).
- **SlateDB / Neon / Turbopuffer**: object-store-as-the-only-state with CAS manifests (`If-None-Match` manifest per version, writer fencing by epoch in manifest) — the same trick as our head/manifest. SlateDB's fencing: writers bump an epoch in the manifest on open; stale writers detect on next CAS and halt. Good validation of the design.
- **Neon safekeeper/pageserver, Aurora**: out of scope but "quorum of safekeepers then lazy materialization to S3" shows sync-quorum with small-latency nodes before async S3 — relevant to the "sync mode but fast" question (a local NVMe journal + peer replicas), noted for other notes.

## 4. Erasure coding across backends vs mirroring

- **Mirroring (N=2/3)**: storage ×N; read from nearest; write amplification ×N; egress for repair = 1× chunk; simplest; failures tolerated N-1. Cross-cloud 2-way mirror costs 2× storage (~$0.023 × 2).
- **EC (k+m) across clouds**, e.g. 2+1 over three clouds: storage ×1.5; tolerates 1 provider outage; reads need k fragments (2 of 3) in parallel — latency = max of k fastest, but can read the nearest 2 and hedge the third; writes need all k+m PUTs (or ack at k+1 with the last async: "write quorum" k+1); small chunks: EC on 64 KiB chunks produces 32 KiB fragments with the per-object request overhead ×1.5; GET cost per read becomes k requests instead of 1 (request costs ×k, latency penalty = slowest of k); **egress on every read** that must fetch k fragments across clouds ($0.09/GB × (data bytes) – the same amount as plain read but every read crosses clouds unless fragments are local: a cold-cache pod in cloud A reads half its data from other clouds); repair after loss = k reads for one fragment rebuilt (more egress); EC works best on large objects (≥ 1 MiB, packs/segments), not 4-64 KiB chunks.
- Verdict: EC across clouds is attractive only for cold archival-ish volumes where storage cost dominates and reads are rare; for a latency-sensitive block device, mirroring with a nearby primary is strictly simpler and faster. Offer EC as a *later per-segment option* ("segments" of ≥ 4-16 MiB; `reed-solomon-erasure` / `isa-l` / `reed-solomon-simd` crates; RS(2,1) is just XOR parity) with failure-domain = backend. Recommendation: do not build in v1; design the manifest to allow a segment to list multiple "location sets" (replica or fragment) so EC can be added without a format break.
- Also note EC within a single provider is pointless (S3 is already 11-nines durable across AZs) — the sole reason to span providers is *provider/region outage and legal/geo* risk, and for migration.

## 5. Credentials and keys in Kubernetes

CSI mechanisms:
- StorageClass `parameters`: `csi.storage.k8s.io/provisioner-secret-name/namespace`, `node-stage-secret-name/namespace`, `node-publish-secret-name/namespace`, `controller-publish-secret-*`, `controller-expand-secret-*`, `snapshotter-secret-*` (in VolumeSnapshotClass). Templates allowed: `${pv.name}`, `${pvc.namespace}`, `${pvc.name}`, `${pvc.annotations['...']}` so each tenant namespace can point to its own secret. Secrets arrive in the gRPC request `secrets` map; kubelet reads them for NodeStage/NodePublish; for static PV: `spec.csi.nodeStageSecretRef`. Important property: kubelet re-reads for NodeStage; rotation without remount needs `requiresRepublish: true` (NodePublish is invoked periodically, ~ every 2 min with `podInfoOnMount`-style republish) -> Lunix can push new credentials to lunixd on each republish (Constellation's `credentials.rs` implements "refresh via Secret watch" as an alternative to republish; that watch needs RBAC `get/list/watch` for secrets `resourceNames` on the node plugin).
- **Cloud workload identity**: IRSA (EKS: ServiceAccount annotated with role ARN + projected OIDC token, AWS SDK `AssumeRoleWithWebIdentity`), EKS Pod Identity (agent), GKE Workload Identity (KSA <-> GSA, or direct resource access via federation), Azure Workload Identity (federated credential, projected token, MSAL). Since `lunixd` is a single DaemonSet pod serving many tenants' volumes, a single node-level identity would give tenants the same S3 permissions: use *per-volume credentials* (secret per PV) for multi-tenant clusters, or restrict the identity to the shared bucket with prefix-scoped IAM policy (`vol/<tenant>/*`) plus per-tenant STS session policies — design: lunixd accepts a **credential provider per backend**: `static (accessKey/secret from secret)`, `web-identity (token file path + role arn)`, `aws-profile/instance-role (IMDS)`, `gcp-adc`, `azure-wi`, `r2-token`. The Rust `aws-config`/`aws-sdk-s3`, `object_store` (supports AWS/GCP/Azure incl. web identity and workload identity), `opendal` crates cover all of these. Cross-cloud case: pod in GKE accessing S3 requires an AWS role trusted via OIDC federation to GKE's issuer (or static keys) — document both.
- **Encryption keys**: per-volume data key (random 256-bit) stored wrapped in the volume header (`head`/`keyring` object) by one or more KEKs: (a) passphrase-derived (argon2id) from a K8s Secret, (b) KMS (AWS KMS / GCP KMS / Azure Key Vault / Vault Transit) — multiple wraps allowed so the volume can be opened in a different cloud using a different KMS (add a wrap with the new KMS before moving, remove old after). Hash-addressing with encryption: dedup requires convergent or keyed-hash (HMAC with a volume-group secret) ids — design note for the crypto track; K8s side: Secret contents never in logs/PV annotations, `zeroize`, `mlock`, core dumps off (Constellation already does `forbid_core_dumps`).
- Where Secrets are NOT to be placed: PV `volumeAttributes` (visible to anyone who can read PVs), pod env, args.
- RBAC and node isolation: node plugin ServiceAccount should only `get` named secrets (resourceNames scoping, as Constellation does) ; kubelet-mediated secret delivery (NodeStage secrets) is least-privilege since kubelet's node authorizer only gives secrets of pods scheduled on that node (for CSI node secrets kubelet uses its own credentials; node authorizer restricts to secrets referenced by PVs of pods on the node: UNVERIFIED detail).
- Rotation: secrets referenced by name; `LunixBackendSet` CRD may have `credentialsRef` and status conditions per backend (`Authenticated`, `HasConditionalWrites`, `Reachable`, `Lag`).

## 6. Recommended architecture

### 6.1 Components

1. **`lunixd`** (data plane; Rust; one per node as DaemonSet, separate from CSI plugin). Owns ublk devices (USER_RECOVERY on), cache, chunk store, backends, lease manager, sync engine. Control API over unix socket (gRPC or Constellation's control framing). Can also run standalone on any Linux box (product requirement) — same binary; the K8s bits are a thin layer.
2. **`lunix-csi`** binary (modes `--controller`, `--node`): Identity+Controller+Node services; node mode talks to lunixd; controller mode talks to S3 directly (create volume = write initial head with `If-None-Match`, snapshots = pin manifests, delete = tombstone head + trash window, expand = new size in manifest) or via a lunixd/`lunix-sync` library — the controller does not need ublk. Shared scaffolding with Constellation's `csi` crate (see 1.4).
3. **`lunix-sync`** (library + CLI/Job): backfill/copy/verify/scrub; invoked by lunixd (tail + incremental) and by Jobs (bulk migration, verify).
4. **CRDs (minimal, optional)**:
   - `LunixBackendSet` (cluster-scoped or namespaced): named list of backends (endpoint, bucket, prefix, region, credential ref/identity mode, role: `authority|mirror|read-only`, egress cost weight). StorageClass references it by name. Status per backend: capability probe results, health, latency.
   - `LunixVolume` (optional, read-mostly mirror of S3 head for observability: owner, epoch, lag per backend, cache stats, last sync); the **source of truth remains S3** — K8s holds only mirrors/hints, so losing a cluster loses nothing. Don't make K8s CRs authoritative (Longhorn's CR-heavy approach ties volumes to a cluster etcd).
   - `LunixMigration`: {volume, add: backendRef, remove: backendRef, mode: preWarm|postMove, bandwidth, budgetUSD}; reconciled by the controller; states Probe -> Joining -> CatchingUp -> InSync -> Verified -> Draining -> Done. v1 can be CLI-only (`lunixctl migrate`).
   - Use standard `VolumeSnapshot*` CRDs for snapshots; `CSIDriver` object for the driver; `StorageClass` for provisioning.
5. **What goes in S3 vs K8s**: S3: head (epoch/owner/lease/backends/cursors), manifests, chunks, snapshot pins, keyring (wrapped keys), tombstones. K8s: StorageClass (parameters: backendSet, mirror policy, mode sync/async, cache size, fs type), Secrets (credentials / passphrase), PV/PVC (volume id, capacity), optional status CRs. Local node: cache dir, journal (write-back), state dir (device<->volume map), no truth.

### 6.2 StorageClass sketch

```yaml
apiVersion: storage.k8s.io/v1
kind: StorageClass
metadata: { name: lunix-global }
provisioner: lunix.replicix.io
parameters:
  backendSet: prod-global          # LunixBackendSet in this cluster
  mirror.mode: primary-async       # sync-all | sync-quorum:2 | primary-async
  replication.maxLag: 30s
  io.mode: write-back              # async mode window
  cache.size: 200Gi
  fsType: ext4
  csi.storage.k8s.io/node-stage-secret-name: lunix-key-${pvc.name}
  csi.storage.k8s.io/node-stage-secret-namespace: ${pvc.namespace}
volumeBindingMode: WaitForFirstConsumer
allowVolumeExpansion: true
reclaimPolicy: Retain
```

### 6.3 Static PV for an imported/moved volume

```yaml
apiVersion: v1
kind: PersistentVolume
metadata: { name: pg-data }
spec:
  capacity: { storage: 500Gi }
  accessModes: [ReadWriteOncePod]
  volumeMode: Block            # or Filesystem
  persistentVolumeReclaimPolicy: Retain
  storageClassName: lunix-global
  csi:
    driver: lunix.replicix.io
    volumeHandle: lunix1:7c1f3f0a-....        # global volume uuid
    volumeAttributes: { backendSet: prod-global }
    nodeStageSecretRef: { name: lunix-key-pg, namespace: db }
```

## 7. Runbook: move a pod from us-east to ap-southeast

Assumptions: cluster E (us-east, AWS, bucket `lunix-use1`), cluster A (ap-southeast, GCP, bucket `lunix-apse` on GCS or S3-compatible), `LunixBackendSet prod-global` defined in both clusters with credentials for both buckets (or each cluster has its own set that includes both). Volume `V` (500 GiB, 180 GiB unique stored) attached to StatefulSet `pg-0` in cluster E.

**Phase 0 – Preflight (no impact)**
1. `lunixctl backend probe gcs://lunix-apse` from cluster E (or via a Job): checks auth, conditional writes (`If-None-Match`, `If-Match` on head-like object), multipart, clock skew, measured RTT/throughput E->A. Abort if conditional writes missing and fencing mode is `strict`.
2. `lunixctl migrate plan V --add apse --remove use1`: prints unique bytes to copy (e.g. 180 GiB), egress estimate (AWS S3 internet egress ~$0.09/GB => ~$16), expected time at the throttle (e.g. 180 GiB at 200 MB/s ≈ 15 min), and checks `backends` capabilities. Requires approval of budget.

**Phase 1 – Add backend live (pod keeps running in E)**
3. `lunixctl backend add V gcs://lunix-apse --role mirror` -> CAS head: `apse = Joining`. Tail replication of new chunks to apse starts immediately (async follower).
4. Backfill runs inside lunixd on the E node (or `LunixMigration` creates a Job `lunix-sync` in E to offload): walk pinned manifests (head + snapshots), copy missing chunks E->A, hot chunks first (hot-set from manifest), bounded bandwidth, AIMD; progress stored in head `backends[apse].backfill`. Observe: `lunixctl backend status V` -> `apse Joining 63% (113/180 GiB) lag 0 gens`.
5. When backfill completes and tail drains: apse -> `CatchingUp` (lag within N gens) -> `InSync`. Run `lunixctl verify V --backend apse --sample 5%` (hash a sample, size-check everything via LIST).

**Phase 2 – Cutover (short pause)**
6. Optionally pre-warm: in cluster A create the static PV/PVC (read-only follower not yet bound), run `lunixctl warm V --backend apse --policy hot` on a node of cluster A: pulls the hot set into that node's cache from the *local* apse bucket (zero egress, fast).
7. Quiesce workload in E: scale StatefulSet `pg` to 0 (graceful: app shuts down, FS unmounts, `NodeUnpublish` -> `NodeUnstage`). `lunixd` flushes dirty cache to E's authority and **all sync/in-sync backends**, publishes final manifest gen `gF` to use1 and apse (I1: chunks first, manifest last), CAS head `released=true`, `epoch=e`. Command waits for `apse.cursor == gF` (`lunixctl wait V --backend apse --gen gF`), typically seconds.
   - Planned moves with no graceful shutdown possible (crash/DR): skip to step 7b.
   - 7b (DR): cluster E is gone/partitioned: in cluster A run `lunixctl volume steal V --from-backend apse --reason "dr"`: reads apse head, picks last published gen (data loss = writes after last async sync, bounded by `replication.maxLag`), waits `lease_ttl + skew` unless `--force`, CAS head on apse with `epoch+1`. If E later returns, its writes fail with 412 and it self-fences (freeze/EIO).
8. If the authority backend is use1 and we want apse to become authority: `lunixctl backend set-authority V apse` (done while released; CAS on use1 head to write `authority=apse, epoch+1`, then publish head on apse). From now on lease CAS targets apse.
9. In cluster A: `kubectl apply` PV/PVC/StatefulSet (via Karmada propagation or Argo CD sync; the same manifests with `volumeHandle: lunix1:<uuid>`). kubelet -> `NodeStageVolume`: lunix-csi-node asks lunixd to `attach V`: reads head from apse (authority), acquires lease (`epoch+1`, owner = cluster A/node), creates ublk device with the pre-warmed cache, mkfs skipped (existing FS detected), mounts, `NodePublish` binds into pod. Pod starts: pause between E shutdown and A start = seconds + app start time.
10. Cold-cache behaviour in A: reads for unwarmed chunks fetch from apse (local region, ~5-20 ms), background prefetch continues; metrics: `lunix_cache_hit_ratio`.

**Phase 3 – Retire the old backend**
11. `lunixctl backend remove V use1 --drain` : `Draining`; barrier gen; verify that apse holds everything (scrub: LIST+size comparison against manifests, optional deep verify with sampling); ensure ≥1 InSync backend remains (policy `min_backends`); CAS head -> `use1 = Removed`. Data in use1 stays untouched unless `--purge` (then delete `vol/<uuid>/` and uniquely-referenced chunks; refuses if other volumes share chunks without refcount info).
12. Delete the PV/PVC/StatefulSet in cluster E (reclaimPolicy Retain at the K8s level: the volume isn't deleted since DeleteVolume isn't called for static PVs with Retain; ensure the E side's PV has `Retain` before deleting the PVC, otherwise `DeleteVolume` would trash the volume! guard: Lunix controller refuses `DeleteVolume` on a volume whose head owner is another cluster or has more than one cluster referenced; also keep the `.trash` window).
13. Post-move: `lunixctl volume status V` shows owner cluster A, epoch, backends=[apse], replication policy; set a new second backend (e.g. another APAC region or back to US) if mirroring should persist.

**Failure branches built into the runbook**
- Backfill stalls (SlowDown 503): AIMD reduces rate; status `Joining (throttled)`; no impact on the running volume beyond bandwidth.
- Network drop during step 7: flush to apse not complete -> `lunixctl wait` times out; do not proceed to unstage-with-release; either retry or `--accept-loss` (documented loss window) — the system refuses to release the lease before the chosen replication policy is satisfied unless forced.
- Step 9 fails (A cannot reach apse): head still `released`; E can re-acquire (epoch+1) and continue; no data loss.
- Old E node comes back after steal: fenced by 412; `lunixd` stops, cache preserved under `quarantine/` for forensics (not auto-replayed).

## Recommendations for Lunix

1. **Process split**: `lunixd` DaemonSet (privileged; `/dev`, `/var/lib/lunix`, cache path) separate from `lunix-csi-node` plugin; plugin restarts/upgrades never touch I/O. Use ublk `USER_RECOVERY` (+`REISSUE`) so `lunixd` crashes/upgrades only stall I/O; spike on Linux 7.0 to measure pause, behaviour with ext4/xfs mounted, and the failure modes with `FAIL_IO`. `OnDelete`/maxUnavailable-1 rollout for the DaemonSet; reuse Constellation's rollout/handoff ideas (state dir lock).
2. **CSI surface v1**: Identity+Controller+Node; `attachRequired: false`; modes Block + Filesystem (ext4/xfs); RWO + RWOP; `EXPAND_VOLUME` online (Controller no-op/size record + Node resize), snapshot + clone via manifest pinning, inline ephemeral local-only volumes, `VOLUME_CONDITION`, `GET_VOLUME_STATS`, no topology constraint (only a primary-backend hint). Pass `csi-sanity`; test against csi-driver-host-path semantics.
3. **Fencing**: require backend conditional writes (`If-None-Match`, `If-Match`); one authority backend per volume; head CAS lease with epoch, TTL 30-60 s, self-fence on lost renewal; operator `steal`; probe at backend-add time. Provide a documented degraded mode for stores without CAS (single-node-only, no automatic failover).
4. **Mirroring**: per-backend cursor over immutable manifest generations; invariant "chunks before manifest, manifest before head"; states `Joining -> CatchingUp -> InSync -> Draining -> Removed`; modes `primary-async` (default), `sync-quorum k/n`, `sync-all`; bounded backfill (hot-first, restartable, AIMD, bandwidth cap, budget); `lunix-sync` as shared library + Job so bulk copies can run off-node.
5. **Reads**: latency/cost-aware backend selection with hedged requests (opt-in cross-cloud hedging because of egress), verify hashes always.
6. **K8s objects**: keep S3 as the sole truth; add only `LunixBackendSet` (config) in v1, `LunixVolume`/`LunixMigration` status CRs in v2; keep volume IDs self-describing (`lunix1:<uuid>` + optional backendSet attribute) so static PVs are portable and tools like Velero/Karmada/Argo can move them.
7. **Pod mobility**: planned move = pre-add destination backend + warm + quiesce + release + acquire (seconds of downtime); unplanned = steal after TTL. Offer a read-only-follower/prewarm mode.
8. **Credentials**: pluggable credential providers per backend (static secret, web identity/IRSA/Pod Identity/GKE WI/Azure WI, IMDS), per-volume secrets for multi-tenant, `requiresRepublish` or Secret-watch for rotation (reuse Constellation `credentials.rs`), multiple KEK wraps of the volume data key (so moving clouds can change the KMS).
9. **Chunk/segment sizing**: copy granularity should be large packs/segments (MiB-scale) to keep request costs and EC options open; per-volume chunk prefix simplifies purge but loses cross-volume dedup — decide jointly with the storage-layout note.
10. **EC**: not in v1; keep manifest locations as a set so replicas/fragments can be added later.
11. **Share code with Constellation**: extract common CSI scaffolding (gRPC services, param parsing, credentials store/refresher, volume-id codec helpers, node GC patterns, rollout controller) into a shared crate; keep engines/control protocols separate.

## Open questions / UNVERIFIED

- ublk on Linux 7.0: exact feature flags (`UBLK_F_USER_RECOVERY`, `_REISSUE`, `_FAIL_IO`, size-update command names), behaviour of a mounted ext4/xfs during server restart, interaction with `hung_task`, and whether ublk devices created in one container are visible/usable from the host mount namespace in all runtimes (containerd/CRI-O, gVisor/Kata nodes will not work). Needs a spike.
- Whether each target backend supports `If-Match` on PUT: confirmed from search/fetch only for AWS S3, R2 (PutObject), partially OVH/Scaleway; GCS/Azure from memory; MinIO, Ceph RGW, Backblaze B2, Wasabi, Hetzner, Tigris: not verified this session.
- R2 does not implement `UploadPartCopy` and bucket replication (CF docs) - server-side copy inside R2 limited to CopyObject (<=5 GiB).
- Pricing figures (egress per GB for AWS/GCP/Azure/B2/Wasabi/OVH/Scaleway/Hetzner) are from memory except AWS inter-region $0.02/GB (example) and GET $0.0004/1000 (AWS page); re-verify.
- Longhorn's ublk frontend status/version; Mayastor frontends; Ceph-CSI fixes to rbd-nbd restart (ceph-csi #5068 status unknown).
- Liqo storage migration capability and exact mechanisms; Karmada failover details with PVs.
- K8s node-authorizer scoping of NodeStage secrets.
- Whether Constellation's `constellation-control` method-table crate is generic enough to share; shared-crate extraction effort.
- Quorum CAS across multiple backends (multi-authority) is only sketched, not designed.
- Azure `Copy Blob From URL` cross-cloud claim: from memory.
- Chunk-level dedup namespace (per-volume vs per-bucket) and its effect on purge/migration: needs the storage-layout decision.

## Sources

- Local: `/home/bra/cvs/replicix/constellation/crates/csi/` (`src/volume_id.rs`, `src/credentials.rs`, `src/node/handoff.rs`, `src/node/{rollout,gc,mounter}.rs`, `src/engine_pods.rs`, `Cargo.toml`), `/home/bra/cvs/replicix/constellation/docs/plans/v1/done/37-kubernetes-csi.md`, `/home/bra/cvs/replicix/constellation/docs/how-to-guides/kubernetes-csi.md`, `/home/bra/cvs/replicix/constellation/docs/explanation/DESIGN.md` (lease/epoch/slot CAS sections), `/home/bra/cvs/replicix/lunix/docs/explanation/research/BRIEF.md`.
- CSI spec: https://github.com/container-storage-interface/spec/blob/master/spec.md
- Kubernetes CSI docs: https://kubernetes-csi.github.io/docs/ ; volume expansion: https://kubernetes-csi.github.io/docs/volume-expansion.html
- Kernel ublk: https://docs.kernel.org/block/ublk.html ; `drivers/block/ublk_drv.c`, `include/uapi/linux/ublk_cmd.h`
- Ceph-CSI rbd-nbd restart issue: https://github.com/ceph/ceph-csi/issues/5068 ; Ceph tracker: https://tracker.ceph.com/issues/51820 ; Rook CSI troubleshooting: https://rook.io/docs/rook/v1.8/ceph-csi-troubleshooting.html ; https://github.com/rook/rook/issues/11575
- S3 conditional writes: https://aws.amazon.com/about-aws/whats-new/2024/11/amazon-s3-functionality-conditional-writes ; https://aws.amazon.com/blogs/storage/building-multi-writer-applications-on-amazon-s3-using-native-controls ; https://morling.dev/blog/leader-election-with-s3-conditional-writes ; OVH: https://docs.ovhcloud.com/en/guides/storage-and-backup/object-storage/s3-conditional-writes ; Scaleway: https://www.scaleway.com/en/docs/object-storage/api-cli/using-conditional-writes/ ; CoreWeave: https://docs.coreweave.com/products/storage/object-storage/buckets/conditional-requests ; Tigris: https://www.tigrisdata.com/blog/s3-conditional-writes/
- Cloudflare R2 S3 API compatibility: https://developers.cloudflare.com/r2/api/s3/api/
- AWS S3 pricing: https://aws.amazon.com/s3/pricing/
- Projects (for prior art; not individually re-fetched): Longhorn (longhorn.io), OpenEBS Mayastor, TopoLVM, kubernetes-csi/csi-driver-host-path, spdk/spdk-csi, Velero, Kasten K10, Karmada, Liqo, Admiralty, Submariner, rclone, restic, Ceph RGW multisite, MinIO site replication, S3 CRR, JuiceFS, Litestream/LiteFS, SlateDB.
- Dean & Barroso, "The Tail at Scale", CACM 2013 (hedged requests).
