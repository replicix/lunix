/* ztime: single-core warm in-memory timing of one arm.  usage: ztime -r recs -f files -m plain|probe|dict|cls -L level [-D name=dict]... [-n passes]
 * Prints one line: mode level bytes compress_s_per_GB(median min max) decompress_s_per_GB(median min max) load1 stored_bytes.
 * plain: zstd level; probe: LZ4 probe of 4 KiB then zstd level unless raw (saves < 6 %); dict: one dictionary; cls: cheap classifier picks
 * the meta/text/bin dictionary (others plain).  The raw guard applies to all (stored raw if not smaller).  Decompress = what the read path does. */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <time.h>
#include <fcntl.h>
#include <unistd.h>
#include <zstd.h>
#include <lz4.h>
#include "cls.h"
typedef struct { uint32_t id, len; uint64_t off; } rec_t;
static double now(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec + t.tv_nsec * 1e-9; }
static void *slurp(const char *p, size_t *n) { FILE *f = fopen(p, "rb"); if (!f) { perror(p); exit(2); } fseek(f, 0, SEEK_END); *n = ftell(f); fseek(f, 0, SEEK_SET); char *b = malloc(*n + 1); if (fread(b, 1, *n, f) != *n) exit(2); b[*n] = 0; fclose(f); return b; }
static int cmpd(const void *a, const void *b) { double x = *(double *)a, y = *(double *)b; return x < y ? -1 : x > y; }
int main(int argc, char **argv) {
  const char *rp = 0, *fp = 0, *mode = "plain"; int level = 1, passes = 7, nd = 0; char *dn[4] = {0}; void *db[4]; size_t dl[4];
  for (int i = 1; i < argc; i++) {
    if (!strcmp(argv[i], "-r")) rp = argv[++i]; else if (!strcmp(argv[i], "-f")) fp = argv[++i]; else if (!strcmp(argv[i], "-m")) mode = argv[++i];
    else if (!strcmp(argv[i], "-L")) level = atoi(argv[++i]); else if (!strcmp(argv[i], "-n")) passes = atoi(argv[++i]);
    else if (!strcmp(argv[i], "-D")) { char *eq = strchr(argv[++i], '='); *eq = 0; dn[nd] = argv[i]; db[nd] = slurp(eq + 1, &dl[nd]); nd++; }
  }
  size_t n; rec_t *recs = slurp(rp, &n); size_t nrec = n / 16; char *ft = slurp(fp, &n); char *files[4096]; int nf = 0; for (char *p = strtok(ft, "\n"); p; p = strtok(0, "\n")) files[nf++] = p;
  uint8_t **raw = malloc(nrec * sizeof *raw); size_t total = 0, maxl = 0;
  for (size_t i = 0; i < nrec; i++) { raw[i] = malloc(recs[i].len); int fd = open(files[recs[i].id], O_RDONLY); if (pread(fd, raw[i], recs[i].len, recs[i].off) != (ssize_t)recs[i].len) exit(3); close(fd); total += recs[i].len; if (recs[i].len > maxl) maxl = recs[i].len; }
  int m_probe = !strcmp(mode, "probe"), m_cls = !strcmp(mode, "cls"), m_dict = !strcmp(mode, "dict");
  ZSTD_CDict *cd[4] = {0}; ZSTD_DDict *dd[4] = {0};
  for (int d = 0; d < nd; d++) { cd[d] = ZSTD_createCDict(db[d], dl[d], level); dd[d] = ZSTD_createDDict(db[d], dl[d]); }
  size_t cap = ZSTD_compressBound(maxl); uint8_t *cb = malloc(cap), *ob = malloc(maxl + 64);
  uint8_t **comp = malloc(nrec * sizeof *comp); size_t *clen = calloc(nrec, sizeof(size_t)); int *dsel = calloc(nrec, sizeof(int)); int *israw = calloc(nrec, sizeof(int));
  ZSTD_CCtx *cc = ZSTD_createCCtx(); ZSTD_DCtx *dc = ZSTD_createDCtx(); double ct[32], dt[32]; size_t stored = 0;
  for (int pass = 0; pass <= passes; pass++) {
    double t0 = now();
    for (size_t i = 0; i < nrec; i++) {
      uint32_t len = recs[i].len; const uint8_t *p = raw[i]; int sel = -1, raw_ = 0;
      if (m_probe) { size_t pl = len < 4096 ? len : 4096; int pc = LZ4_compress_default((const char *)p, (char *)cb, pl, cap); if (pc <= 0 || pc * 100 > (int)pl * 94) raw_ = 1; }
      if (m_cls) { int c = cheap_class(p, len); sel = c == 1 ? 0 : c == 2 ? 1 : c == 3 ? 2 : -1; }
      if (m_dict) sel = 0;
      size_t z = 0;
      if (!raw_) {
        ZSTD_CCtx_reset(cc, ZSTD_reset_session_and_parameters); ZSTD_CCtx_setParameter(cc, ZSTD_c_dictIDFlag, 0);
        if (sel >= 0) ZSTD_CCtx_refCDict(cc, cd[sel]); else ZSTD_CCtx_setParameter(cc, ZSTD_c_compressionLevel, level);
        z = ZSTD_compress2(cc, cb, cap, p, len);
      }
      if (pass == 0) { israw[i] = raw_ || ZSTD_isError(z) || z >= len; dsel[i] = sel; clen[i] = israw[i] ? len : z; comp[i] = israw[i] ? NULL : memcpy(malloc(z), cb, z); stored += clen[i]; }
    }
    if (pass) ct[pass - 1] = now() - t0;
  }
  for (int pass = 0; pass <= passes; pass++) {
    double t0 = now();
    for (size_t i = 0; i < nrec; i++) {
      if (israw[i]) { memcpy(ob, raw[i], recs[i].len); continue; }      /* raw chunk: a copy */
      ZSTD_DCtx_reset(dc, ZSTD_reset_session_only); if (dsel[i] >= 0) ZSTD_DCtx_refDDict(dc, dd[dsel[i]]); else ZSTD_DCtx_refDDict(dc, NULL);
      ZSTD_decompressDCtx(dc, ob, recs[i].len + 64, comp[i], clen[i]) ; if (0) ZSTD_decompressDCtx(dc, ob, 1, 0, 0);
    }
    if (pass) dt[pass - 1] = now() - t0;
  }
  double gb = total / 1e9; qsort(ct, passes, sizeof(double), cmpd); qsort(dt, passes, sizeof(double), cmpd);
  double l1 = 0; FILE *lf = fopen("/proc/loadavg", "r"); if (lf) { fscanf(lf, "%lf", &l1); fclose(lf); }
  printf("%s\t%d\t%zu\t%.4f\t%.4f\t%.4f\t%.4f\t%.4f\t%.4f\t%.2f\t%zu\n", mode, level, total, ct[passes / 2] / gb, ct[0] / gb, ct[passes - 1] / gb, dt[passes / 2] / gb, dt[0] / gb, dt[passes - 1] / gb, l1, stored);
  return 0;
}
