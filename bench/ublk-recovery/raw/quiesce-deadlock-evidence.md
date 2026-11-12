# Evidence: STOP_DEV after QUIESCE deadlocks the daemon (2026-11-11/12, kernel 7.0.0 Ubuntu)

Setup: ext4 mounted on a 4-queue recoverable device, fio verify + 2 fsync writers + 1 ms heartbeat running.
The daemon received SIGUSR2, called `UblkCtrl::quiesce_dev(5000)` (returned `Ok(0)`), and then returned from
libublk's `run_target()`, which unconditionally issues `STOP_DEV` after the queue threads exit.

State 10 minutes later (all processes in uninterruptible sleep, `kill -9` of the daemon has no effect):

    daemon main thread:  io_cqring_wait            (waiting for the STOP_DEV completion)
    io-wq worker thread: D, msleep
        ublk_stop_dev_unlocked+0x115 [ublk_drv]     (ublk_force_abort_dev -> ublk_wait_tagset_rqs_idle: msleep loop
        ublk_ctrl_uring_cmd+0x60b [ublk_drv]         until no started request remains)
        io_uring_cmd -> io_wq_submit_work -> io_wq_worker
    ext4 writers (fio, python fsyncers, heartbeat, jbd2, flush kworker): D in jbd2/ext4 paths

Reading: the STOP_DEV runs while requests are still "started" (handed to the departed server); they are only
requeued by ublk_ch_release() when the last reference to /dev/ublkcN is dropped, which needs the daemon to exit,
but the daemon cannot exit while its io-wq worker is blocked in the STOP_DEV command. Self-deadlock; only a reboot
(not permitted on this host) or an unlikely driver-side completion releases it. Not retried, nothing forced.
Device 0 stays registered in created-devices. Fix in the spike (untested, see RESULTS.md): exit the process
right after a successful quiesce instead of returning into run_target().
