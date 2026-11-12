# I/O while the server is away

## fail-io (dev_info flags: flags=0x2764a)
- fio ended 150 ms after the server was killed, exit code 1; fio: io_u error on file /dev/ublkb0: Input/output error: write offset=842719232, buflen=4096;fio: io_u error on file /dev/ublkb0: Input/output error: write offset=2101342208, buflen=4096;
- recovery failed
- recovered after  ms (exec to END_USER_RECOVERY)
- final fio exit code: 1; json errors: [null]

## reissue (dev_info flags: flags=0x2745a)
- 8 s after kill -9 fio is still running with no error (I/O queued while the server is away)
- recovered after 65 ms (exec to END_USER_RECOVERY)
- final fio exit code: 0; json errors: [null]
