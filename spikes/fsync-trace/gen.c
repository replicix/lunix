/* Load generators for the fsync-trace spike. Build: cc -O2 -o gen gen.c */
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static uint64_t now_ns(void) {
	struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t);
	return (uint64_t)t.tv_sec * 1000000000ull + t.tv_nsec;
}
static void sleep_until(uint64_t ns) {
	struct timespec t = { ns / 1000000000ull, ns % 1000000000ull };
	clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &t, NULL);
}
static int cmpu(const void *a, const void *b) {
	uint64_t x = *(const uint64_t *)a, y = *(const uint64_t *)b;
	return x < y ? -1 : x > y;
}
static void report(const char *name, uint64_t *lat, long n, double secs, double bytes) {
	qsort(lat, n, sizeof *lat, cmpu);
	printf("%s ops=%ld ops_per_s=%.1f MBps=%.1f lat_us_p50=%lu p99=%lu max=%lu\n", name, n, n / secs,
	       bytes / secs / 1e6, n ? lat[n / 2] / 1000 : 0, n ? lat[n * 99 / 100] / 1000 : 0, n ? lat[n - 1] / 1000 : 0);
}
static void die(const char *m) { perror(m); exit(1); }

int main(int argc, char **argv) {
	if (argc < 3) { fprintf(stderr, "usage: gen prep|small|stream|dsync ...\n"); return 2; }
	const char *mode = argv[1], *path = argv[2];
	if (!strcmp(mode, "prep")) { /* prep path MiB : write zeros, fsync */
		long mib = atol(argv[3]);
		int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
		if (fd < 0) die("open");
		if (fallocate(fd, 0, 0, mib << 20)) perror("fallocate");
		char *b = calloc(1, 1 << 20);
		for (long i = 0; i < mib; i++) if (pwrite(fd, b, 1 << 20, (off_t)i << 20) != 1 << 20) die("pwrite");
		fsync(fd); close(fd); return 0;
	}
	double secs = atof(argv[3]);
	long cap = (long)(secs * 100000) + 1000;
	uint64_t *lat = malloc(cap * sizeof *lat);
	uint64_t t0 = now_ns(), end = t0 + (uint64_t)(secs * 1e9);
	long n = 0;
	if (!strcmp(mode, "small")) { /* small path secs rate fsync|fdatasync */
		double rate = atof(argv[4]); int data = !strcmp(argv[5], "fdatasync");
		int fd = open(path, O_WRONLY | O_CREAT | O_APPEND, 0644);
		if (fd < 0) die("open");
		char buf[4096]; memset(buf, 'S', sizeof buf);
		uint64_t period = (uint64_t)(1e9 / rate), next = t0;
		while (next < end && n < cap) {
			sleep_until(next);
			uint64_t a = now_ns();
			if (write(fd, buf, sizeof buf) != sizeof buf) die("write");
			if (data ? fdatasync(fd) : fsync(fd)) die("sync");
			lat[n++] = now_ns() - a; next += period;
		}
		report("small", lat, n, secs, n * 4096.0);
	} else if (!strcmp(mode, "stream")) { /* stream path secs MBps fileMiB [wb]: buffered, cyclic overwrite, never fsync.
		wb = start writeback 8 MiB behind the writer with sync_file_range(WRITE) (no wait, no flush) */
		double mbps = atof(argv[4]); long fmib = atol(argv[5]); int wb = argc > 6 && !strcmp(argv[6], "wb");
		int fd = open(path, O_WRONLY | O_CREAT, 0644);
		if (fd < 0) die("open");
		char *b = malloc(1 << 20); memset(b, 'T', 1 << 20);
		double bytes = 0;
		while (now_ns() < end) {
			uint64_t a = now_ns();
			if (pwrite(fd, b, 1 << 20, (off_t)((n % fmib) << 20)) != 1 << 20) die("pwrite");
			if (wb && n >= 8) sync_file_range(fd, (off_t)(((n - 8) % fmib) << 20), 1 << 20, SYNC_FILE_RANGE_WRITE);
			lat[n < cap ? n : cap - 1] = now_ns() - a; n++; bytes += 1 << 20;
			uint64_t due = t0 + (uint64_t)(bytes / (mbps * 1e6) * 1e9);
			if (due > now_ns()) sleep_until(due);
		}
		report("stream", lat, n < cap ? n : cap, secs, bytes);
	} else if (!strcmp(mode, "dsync")) { /* dsync path secs rate fileMiB ow|append */
		double rate = atof(argv[4]); long fmib = atol(argv[5]); int app = !strcmp(argv[6], "append");
		int fd = open(path, O_WRONLY | O_CREAT | O_DIRECT | O_DSYNC, 0644);
		if (fd < 0) die("open");
		char *buf; if (posix_memalign((void **)&buf, 4096, 4096)) die("memalign");
		memset(buf, 'D', 4096);
		uint64_t period = (uint64_t)(1e9 / rate), next = t0, rng = 88172645463325252ull;
		long pages = (fmib << 20) / 4096;
		while (next < end && n < cap) {
			sleep_until(next);
			rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17;
			off_t off = app ? (off_t)n * 4096 : (off_t)(rng % pages) * 4096;
			uint64_t a = now_ns();
			if (pwrite(fd, buf, 4096, off) != 4096) die("pwrite");
			lat[n++] = now_ns() - a; next += period;
		}
		report(app ? "dsync-append" : "dsync-ow", lat, n, secs, n * 4096.0);
	} else return 2;
	return 0;
}
