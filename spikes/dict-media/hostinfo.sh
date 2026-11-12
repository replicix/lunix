#!/bin/sh
echo "AMD EPYC (Zen 5), 16 vCPUs, SMT off, 1 NUMA node, ~30 GiB RAM, local NVMe SSD, ext4 root"
echo "kernel $(uname -r | cut -d- -f1) (Ubuntu), $(. /etc/os-release; echo $PRETTY_NAME)"
echo "python $(python3 -V | cut -d' ' -f2), zstandard $(python3 -c 'import zstandard as z;print(z.__version__, z.ZSTD_VERSION)'), lz4 $(python3 -c 'import lz4;print(lz4.__version__)')"
echo "imagemagick $(convert -version | head -1 | cut -d' ' -f3), ffmpeg $(ffmpeg -version | head -1 | cut -d' ' -f3), 7z $(7z | sed -n 2p | cut -d' ' -f3)"
