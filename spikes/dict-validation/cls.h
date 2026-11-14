/* Cheap chunk classifier (stand-in for ADR-30's: magic, text/binary, byte entropy).  Classes:
 * 0 empty, 1 meta (file-system structure magic / inode table), 2 text, 3 binary, 4 unclassified (high entropy: compressed or encrypted). */
#include <stdint.h>
#include <string.h>
#include <math.h>
static inline uint32_t rd32(const uint8_t *p) { uint32_t v; memcpy(&v, p, 4); return v; }
static inline uint64_t rd64(const uint8_t *p) { uint64_t v; memcpy(&v, p, 8); return v; }
static inline uint16_t rd16(const uint8_t *p) { uint16_t v; memcpy(&v, p, 2); return v; }
/* one 4 KiB block looks like a file-system structure block */
static int meta_block(const uint8_t *b) {
  uint32_t m = rd32(b);
  static const char *x[] = {"XFSB","XAGF","XAGI","XAFL","ABTB","ABTC","IABT","FIBT","AB3B","AB3C","IAB3","FIB3","BMAP","BMA3","XDB3","XDD3","XDF3","XD2B","XD2D","XD2F","XSLM","XFSD","XLOG","RMB3","RMAP","CRC3",0};
  for (int i = 0; x[i]; i++) if (m == rd32((const uint8_t *)x[i])) return 1;
  if (b[0] == 'I' && b[1] == 'N' && (b[2] | b[3] << 8) != 0 && (b[2] == 0x81 || b[2] == 0x41 || b[2] == 0xa1)) return 1;  /* xfs inode (mode high byte) */
  if (b[0] == 0x98 && b[1] == 0x39 && b[2] == 0x3b && b[3] == 0xc0) return 1;              /* jbd2 magic C03B3998 big-endian */
  if (b[0] == 0x0a && b[1] == 0xf3) return 1;                                                 /* ext4 extent node */
  if (rd16(b + 0x438) == 0xEF53) return 1;                                                    /* ext4 superblock */
  if (!memcmp(b + 0x40, "_BHRfS_M", 8)) return 1;                                             /* btrfs superblock */
  if (!memcmp(b, "_BHRfS_M", 8)) return 1;
  /* btrfs tree block header: bytenr (LE u64, nodesize-aligned) at 0x30, level at 0x64 < 8, nritems at 0x60 sane, owner 0x58 sane */
  { uint64_t by = rd64(b + 0x30), ow = rd64(b + 0x58); uint32_t nr = rd32(b + 0x60);
    if (by && (by & 0x3fff) == 0 && b[0x64] < 8 && nr < 2000 && ow > 0 && ow < 1024 && rd64(b + 0x50) < (1ull << 40)) return 1; }
  /* ext4 inode table: 256-byte inodes with i_extra_isize == 32 at +0x80 */
  { int hit = 0, tot = 0; for (int s = 0; s < 16; s++) { const uint8_t *in = b + s * 256; int nz = 0; for (int k = 0; k < 8; k++) nz |= in[k]; if (!nz) continue; tot++; if (rd16(in + 0x80) == 32 && (in[1] & 0xf0)) hit++; }
    if (tot >= 2 && hit * 2 >= tot) return 1; }
  return 0;
}
static inline int cheap_class(const uint8_t *p, size_t n) {
  size_t nb = n / 4096; if (nb == 0) nb = 1; int nz = 0, mt = 0;
  for (size_t i = 0; i < nb; i++) { const uint8_t *b = p + i * 4096; if (n - i * 4096 < 4096) break; int any = 0; for (int k = 0; k < 4096; k += 8) if (rd64(b + k)) { any = 1; break; } if (!any) continue; nz++; mt += meta_block(b); }
  if (nz == 0) return 0; if (mt * 2 >= nz) return 1;
  size_t hl = n < 4096 ? n : 4096; unsigned cnt[256] = {0}; for (size_t i = 0; i < hl; i++) cnt[p[i]]++;
  double H = 0; for (int i = 0; i < 256; i++) if (cnt[i]) { double q = (double)cnt[i] / hl; H -= q * log2(q); }
  unsigned tx = 0; for (int i = 0; i < 256; i++) if (i == 9 || i == 10 || i == 13 || (i >= 32 && i < 127) || i >= 0xc2) tx += cnt[i];
  if (H >= 7.2) return 4;
  if (cnt[0] == 0 && tx * 100 >= hl * 97 && H < 6.5) return 2;
  return 3;
}
