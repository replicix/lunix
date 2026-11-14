/* zmeasure: per-record compressed sizes for plain zstd and dictionary arms, LZ4 probe and cheap class.
 * Usage: zmeasure -r recs.bin -f files.txt -o out.bin -L 1,3 [-D name=dict.bin ...] [-t threads]
 * recs.bin: records of {u32 file_id, u64 offset, u32 len} (packed, 16 bytes: u32 id, u32 len, u64 off).
 * files.txt: one path per line (file_id = line number). out.bin: per record u32 columns:
 *   [0] lz4 probe size of first min(4096,len) bytes, [1] cheap class, then for each level: plain, each dict.
 * Frames: dictID flag off, content size on, no checksum. */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <fcntl.h>
#include <unistd.h>
#include <pthread.h>
#include <stdatomic.h>
#include <zstd.h>
#include <lz4.h>
#include "cls.h"
typedef struct { uint32_t id, len; uint64_t off; } rec_t;
static rec_t *recs; static size_t nrec; static char **files; static size_t nfiles;
static int ndict, nlev, levels[8]; static char *dnames[64]; static ZSTD_CDict *cd[8][64]; static void *dbuf[64]; static size_t dlen[64];
static uint32_t dmask[64]; static int skipraw, noplain; static uint32_t *outv; static int ncol; static atomic_size_t nextrec; static size_t maxlen;
static void *worker(void *arg) {
  (void)arg; uint8_t *buf = malloc(maxlen); size_t cap = ZSTD_compressBound(maxlen); uint8_t *cb = malloc(cap);
  ZSTD_CCtx *cc = ZSTD_createCCtx(); int fd = -1; uint32_t curid = UINT32_MAX;
  for (;;) {
    size_t b = atomic_fetch_add(&nextrec, 64); if (b >= nrec) break;
    size_t e = b + 64 < nrec ? b + 64 : nrec;
    for (size_t i = b; i < e; i++) {
      rec_t *r = &recs[i]; uint32_t *o = outv + i * ncol;
      if (curid != r->id) { if (fd >= 0) close(fd); fd = open(files[r->id], O_RDONLY); curid = r->id; if (fd < 0) { perror(files[r->id]); exit(2); } }
      size_t got = 0; while (got < r->len) { ssize_t k = pread(fd, buf + got, r->len - got, r->off + got); if (k <= 0) break; got += k; }
      if (got != r->len) { fprintf(stderr, "short read %s\n", files[r->id]); exit(2); }
      size_t pl = r->len < 4096 ? r->len : 4096; int pc = LZ4_compress_default((char *)buf, (char *)cb, pl, cap); o[0] = pc > 0 ? pc : pl;
      o[1] = cheap_class(buf, r->len); int praw = (uint64_t)o[0] * 100 >= 94ull * pl;   /* ADR-12 probe: raw if it saves under 6 % */
      int c = 2;
      for (int l = 0; l < nlev; l++) {
        ZSTD_CCtx_reset(cc, ZSTD_reset_session_and_parameters);
        ZSTD_CCtx_setParameter(cc, ZSTD_c_compressionLevel, levels[l]); ZSTD_CCtx_setParameter(cc, ZSTD_c_dictIDFlag, 0);
        size_t z = 0; if (noplain) o[c++] = UINT32_MAX; else { z = ZSTD_compress2(cc, cb, cap, buf, r->len); o[c++] = ZSTD_isError(z) ? r->len : z; }
        for (int d = 0; d < ndict; d++) {
          if (!((dmask[d] >> (o[1] & 31)) & 1) || (skipraw && praw)) { o[c++] = UINT32_MAX; continue; }   /* not evaluated: wrong class, or probe-raw chunk */
          ZSTD_CCtx_reset(cc, ZSTD_reset_session_and_parameters); ZSTD_CCtx_setParameter(cc, ZSTD_c_dictIDFlag, 0);
          ZSTD_CCtx_refCDict(cc, cd[l][d]); z = ZSTD_compress2(cc, cb, cap, buf, r->len); o[c++] = ZSTD_isError(z) ? r->len : z;
        }
      }
    }
  }
  if (fd >= 0) close(fd); ZSTD_freeCCtx(cc); free(buf); free(cb); return NULL;
}
static void *slurp(const char *p, size_t *n) { FILE *f = fopen(p, "rb"); if (!f) { perror(p); exit(2); } fseek(f, 0, SEEK_END); *n = ftell(f); fseek(f, 0, SEEK_SET); void *b = malloc(*n + 1); if (fread(b, 1, *n, f) != *n) exit(2); fclose(f); return b; }
int main(int argc, char **argv) {
  const char *rp = 0, *fp = 0, *op = 0; int nthr = 4; char *lv = "1,3";
  for (int i = 1; i < argc; i++) {
    if (!strcmp(argv[i], "-r")) rp = argv[++i]; else if (!strcmp(argv[i], "-f")) fp = argv[++i]; else if (!strcmp(argv[i], "-o")) op = argv[++i];
    else if (!strcmp(argv[i], "-L")) lv = argv[++i]; else if (!strcmp(argv[i], "-t")) nthr = atoi(argv[++i]);
    else if (!strcmp(argv[i], "-P")) skipraw = 1; else if (!strcmp(argv[i], "-n")) noplain = 1;
    else if (!strcmp(argv[i], "-D")) { char *eq = strchr(argv[++i], '='); *eq = 0; dnames[ndict] = argv[i]; char *co = strrchr(eq + 1, ':'); dmask[ndict] = 0xffffffffu;
      if (co) { *co = 0; dmask[ndict] = 0; for (char *t = strtok(co + 1, ","); t; t = strtok(0, ",")) dmask[ndict] |= 1u << atoi(t); }
      dbuf[ndict] = slurp(eq + 1, &dlen[ndict]); ndict++; }
  }
  if (!rp || !fp || !op) { fprintf(stderr, "usage\n"); return 1; }
  for (char *t = strtok(strdup(lv), ","); t; t = strtok(0, ",")) levels[nlev++] = atoi(t);
  size_t n; recs = slurp(rp, &n); nrec = n / sizeof(rec_t); if (sizeof(rec_t) != 16) return 3;
  char *ft = slurp(fp, &n); ft[n] = 0; files = malloc(sizeof(char *) * (n + 1)); nfiles = 0;
  for (char *p = strtok(ft, "\n"); p; p = strtok(0, "\n")) files[nfiles++] = p;
  maxlen = 0; for (size_t i = 0; i < nrec; i++) { if (recs[i].len > maxlen) maxlen = recs[i].len; if (recs[i].id >= nfiles) return 4; }
  for (int l = 0; l < nlev; l++) for (int d = 0; d < ndict; d++) cd[l][d] = ZSTD_createCDict(dbuf[d], dlen[d], levels[l]);
  ncol = 2 + nlev * (1 + ndict); outv = calloc(nrec * ncol, 4);
  pthread_t th[32]; if (nthr > 32) nthr = 32; for (int i = 0; i < nthr; i++) pthread_create(&th[i], 0, worker, 0);
  for (int i = 0; i < nthr; i++) pthread_join(th[i], 0);
  FILE *f = fopen(op, "wb"); fwrite(outv, 4, nrec * ncol, f); fclose(f);
  char cp[4096]; snprintf(cp, sizeof cp, "%s.cols", op); f = fopen(cp, "w"); fprintf(f, "probe\nclass\n");
  for (int l = 0; l < nlev; l++) { fprintf(f, "z%d\n", levels[l]); for (int d = 0; d < ndict; d++) fprintf(f, "%s@%d\n", dnames[d], levels[l]); } fclose(f);
  fprintf(stderr, "zmeasure: %zu records, %d columns\n", nrec, ncol); return 0;
}
