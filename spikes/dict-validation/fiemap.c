/* fiemap: walk a mounted tree (crosses btrfs subvolume boundaries; nothing else is mounted below the labelling mount) and print the physical extents of every regular file and directory.
 * Output lines (tab separated): "P id kind size ino path" then "E id phys_bytes len_bytes flags".  kind: F or D.
 * Usage: fiemap <mountpoint>  (read-only; run as root so every file can be opened). */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <fcntl.h>
#include <unistd.h>
#include <ftw.h>
#include <errno.h>
#include <sys/ioctl.h>
#include <linux/fs.h>
#include <linux/fiemap.h>
static unsigned long next_id;
static int visit(const char *path, const struct stat *st, int tf, struct FTW *fw) {
  (void)fw;
  int dir = tf == FTW_D || tf == FTW_DP; if (!(dir || tf == FTW_F)) return 0;
  if (!dir && !S_ISREG(st->st_mode)) return 0;
  int fd = open(path, O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_NOATIME); if (fd < 0) fd = open(path, O_RDONLY | O_NOFOLLOW | O_NONBLOCK); if (fd < 0) return 0;
  unsigned long id = next_id++; int cap = 256; struct fiemap *fm = malloc(sizeof *fm + cap * sizeof(struct fiemap_extent));
  printf("P\t%lu\t%c\t%lld\t%llu\t%s\n", id, dir ? 'D' : 'F', (long long)st->st_size, (unsigned long long)st->st_ino, path);
  uint64_t start = 0;
  for (;;) {
    memset(fm, 0, sizeof *fm); fm->fm_start = start; fm->fm_length = ~0ULL - start; fm->fm_extent_count = cap;
    if (ioctl(fd, FS_IOC_FIEMAP, fm) < 0) break; if (!fm->fm_mapped_extents) break;
    int last = 0;
    for (unsigned i = 0; i < fm->fm_mapped_extents; i++) {
      struct fiemap_extent *e = &fm->fm_extents[i];
      if (!(e->fe_flags & (FIEMAP_EXTENT_DATA_INLINE | FIEMAP_EXTENT_NOT_ALIGNED)) || (e->fe_flags & FIEMAP_EXTENT_NOT_ALIGNED))
        if (!(e->fe_flags & FIEMAP_EXTENT_DATA_INLINE)) printf("E\t%lu\t%llu\t%llu\t%u\n", id, (unsigned long long)e->fe_physical, (unsigned long long)e->fe_length, e->fe_flags);
      start = e->fe_logical + e->fe_length; if (e->fe_flags & FIEMAP_EXTENT_LAST) last = 1;
    }
    if (last) break;
  }
  free(fm); close(fd); return 0;
}
int main(int argc, char **argv) { if (argc < 2) return 1; nftw(argv[1], visit, 64, FTW_PHYS); return 0; }
