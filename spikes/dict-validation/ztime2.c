/* ztime2: single-core, warm, in-memory timing of one arm (CPU cost, section 1.1 / "Timing" of the protocol).  Successor of ztime.c.
 * usage: ztime2 -r recs -s sample.bin -L level [-P] [-C] [-S sel.u8] [-D name=dict]... [-n passes] [-R raw.u8]
 *   recs     16-byte records {u32 id (ignored), u32 len, u64 off} into sample.bin (the 1 GiB in-memory sample)
 *   -L       zstd level (plain compression level, or the level the dictionaries are digested at)
 *   -P       ADR-12 probe first: LZ4 of the first 4 KiB, chunk stored raw if it saves under 6 % (raw chunks cost the probe only)
 *   -C       cheap classifier picks the dictionary: class meta/text/bin -> -D #0/#1/#2, other classes plain (bs-* and pv-class arms)
 *   -S file  one byte per record: 0 plain, k = dictionary k-1 (zdicts `ft` selection computed by zdtool, timed separately)
 *   -D       dictionary (several allowed; name=- is an empty slot); without -C/-S the first one is used for every record (pv, db arms)
 *   -Q file  write one byte per record (1 = the probe sent it raw: no classifier, no dictionary, no zstd) after the first pass
 *   -R file  write one byte per record (1 = stored raw by the probe or because zstd did not shrink it) after the first pass
 * The raw guard (stored raw if not smaller) applies to every arm.  Decompress = what the read path does (raw chunk: a copy; else
 * ZSTD_decompressDCtx with the chunk's DDict).  Each pass is timed as a whole; 1 warm-up pass, then `passes` timed passes.
 * Output, one TSV line: level bytes comp_s_per_GB(median min max) decomp_s_per_GB(median min max) load1 stored_bytes raw_chunks chunks */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <time.h>
#include <zstd.h>
#include <lz4.h>
#include "cls.h"
typedef struct { uint32_t id, len; uint64_t off; } rec_t;
static double now(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec + t.tv_nsec * 1e-9; }
static void *slurp(const char *p, size_t *n) { FILE *f = fopen(p, "rb"); if (!f) { perror(p); exit(2); } fseek(f, 0, SEEK_END); *n = ftell(f); fseek(f, 0, SEEK_SET); char *b = malloc(*n + 1); if (fread(b, 1, *n, f) != *n) exit(2); b[*n] = 0; fclose(f); return b; }
static int cmpd(const void *a, const void *b) { double x = *(double *)a, y = *(double *)b; return x < y ? -1 : x > y; }
#define MAXD 64
int main(int argc, char **argv) {
  const char *rp = 0, *sp = 0, *selp = 0, *rawp = 0, *probep = 0; int level = 1, passes = 7, nd = 0, probe = 0, cls = 0; void *db[MAXD]; size_t dl[MAXD];
  for (int i = 1; i < argc; i++) {
    if (!strcmp(argv[i], "-r")) rp = argv[++i]; else if (!strcmp(argv[i], "-s")) sp = argv[++i]; else if (!strcmp(argv[i], "-S")) selp = argv[++i];
    else if (!strcmp(argv[i], "-R")) rawp = argv[++i]; else if (!strcmp(argv[i], "-Q")) probep = argv[++i]; else if (!strcmp(argv[i], "-L")) level = atoi(argv[++i]); else if (!strcmp(argv[i], "-n")) passes = atoi(argv[++i]);
    else if (!strcmp(argv[i], "-P")) probe = 1; else if (!strcmp(argv[i], "-C")) cls = 1;
    else if (!strcmp(argv[i], "-D")) { char *eq = strchr(argv[++i], '='); if (!eq || nd >= MAXD) exit(2); if (!strcmp(eq + 1, "-")) { db[nd] = 0; dl[nd] = 0; } else db[nd] = slurp(eq + 1, &dl[nd]); nd++; }   /* "-" = no dictionary in this slot */
  }
  if (!rp || !sp || passes < 1 || passes > 31) { fprintf(stderr, "usage\n"); return 2; }
  size_t n, ssz; rec_t *recs = slurp(rp, &n); size_t nrec = n / 16; uint8_t *sample = slurp(sp, &ssz);
  uint8_t *fsel = selp ? slurp(selp, &n) : 0; if (fsel && n != nrec) { fprintf(stderr, "selection length mismatch\n"); return 2; }
  uint8_t **raw = malloc(nrec * sizeof *raw); size_t total = 0, maxl = 0;
  for (size_t i = 0; i < nrec; i++) { if (recs[i].off + recs[i].len > ssz) exit(3); raw[i] = malloc(recs[i].len); memcpy(raw[i], sample + recs[i].off, recs[i].len); total += recs[i].len; if (recs[i].len > maxl) maxl = recs[i].len; }
  free(sample);
  ZSTD_CDict *cd[MAXD] = {0}; ZSTD_DDict *dd[MAXD] = {0};
  for (int d = 0; d < nd; d++) if (db[d]) { cd[d] = ZSTD_createCDict(db[d], dl[d], level); dd[d] = ZSTD_createDDict(db[d], dl[d]); }
  size_t cap = ZSTD_compressBound(maxl); uint8_t *cb = malloc(cap), *ob = malloc(maxl + 64);
  uint8_t **comp = malloc(nrec * sizeof *comp); size_t *clen = calloc(nrec, sizeof(size_t)); int *dsel = calloc(nrec, sizeof(int)); uint8_t *israw = calloc(nrec, 1), *praw = calloc(nrec, 1);
  ZSTD_CCtx *cc = ZSTD_createCCtx(); ZSTD_DCtx *dc = ZSTD_createDCtx(); double ct[32], dt[32]; size_t stored = 0, nraw = 0;
  for (int pass = 0; pass <= passes; pass++) {
    double t0 = now();
    for (size_t i = 0; i < nrec; i++) {
      uint32_t len = recs[i].len; const uint8_t *p = raw[i]; int sel = -1, raw_ = 0;
      if (probe) { size_t pl = len < 4096 ? len : 4096; int pc = LZ4_compress_default((const char *)p, (char *)cb, pl, cap); if (pc <= 0 || pc * 100 > (int)pl * 94) raw_ = 1; }
      size_t z = 0;
      if (!raw_) {
        if (cls) { int c = cheap_class(p, len); sel = c == 1 ? 0 : c == 2 ? 1 : c == 3 ? 2 : -1; if (sel >= nd) sel = -1; }
        else if (fsel) sel = fsel[i] ? (int)fsel[i] - 1 : -1;
        else if (nd) sel = 0;
        if (sel >= 0 && !cd[sel]) sel = -1;
        ZSTD_CCtx_reset(cc, ZSTD_reset_session_and_parameters); ZSTD_CCtx_setParameter(cc, ZSTD_c_dictIDFlag, 0);
        if (sel >= 0) ZSTD_CCtx_refCDict(cc, cd[sel]); else ZSTD_CCtx_setParameter(cc, ZSTD_c_compressionLevel, level);
        z = ZSTD_compress2(cc, cb, cap, p, len);
      }
      if (pass == 0) { praw[i] = (uint8_t)raw_; israw[i] = raw_ || ZSTD_isError(z) || z >= len; dsel[i] = sel; clen[i] = israw[i] ? len : z; comp[i] = israw[i] ? NULL : memcpy(malloc(z), cb, z); stored += clen[i]; nraw += israw[i]; }
    }
    if (pass) ct[pass - 1] = now() - t0;
  }
  for (int pass = 0; pass <= passes; pass++) {
    double t0 = now();
    for (size_t i = 0; i < nrec; i++) {
      if (israw[i]) { memcpy(ob, raw[i], recs[i].len); continue; }      /* raw chunk: a copy */
      ZSTD_DCtx_reset(dc, ZSTD_reset_session_only); ZSTD_DCtx_refDDict(dc, dsel[i] >= 0 ? dd[dsel[i]] : NULL);
      size_t r = ZSTD_decompressDCtx(dc, ob, recs[i].len + 64, comp[i], clen[i]);
      if (ZSTD_isError(r) || r != recs[i].len) { fprintf(stderr, "decompress mismatch at record %zu\n", i); return 4; }
    }
    if (pass) dt[pass - 1] = now() - t0;
  }
  if (probep) { FILE *f = fopen(probep, "wb"); fwrite(praw, 1, nrec, f); fclose(f); }
  if (rawp) { FILE *f = fopen(rawp, "wb"); fwrite(israw, 1, nrec, f); fclose(f); }
  double gb = total / 1e9; qsort(ct, passes, sizeof(double), cmpd); qsort(dt, passes, sizeof(double), cmpd);
  double l1 = 0; FILE *lf = fopen("/proc/loadavg", "r"); if (lf) { if (fscanf(lf, "%lf", &l1) != 1) l1 = -1; fclose(lf); }
  printf("%d\t%zu\t%.4f\t%.4f\t%.4f\t%.4f\t%.4f\t%.4f\t%.2f\t%zu\t%zu\t%zu\n", level, total, ct[passes / 2] / gb, ct[0] / gb, ct[passes - 1] / gb, dt[passes / 2] / gb, dt[0] / gb, dt[passes - 1] / gb, l1, stored, nraw, nrec);
  return 0;
}
