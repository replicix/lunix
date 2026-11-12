#!/bin/sh
# Prints only the categories allowed in result files.
echo "CPU: AMD EPYC (Zen 5), 16 vCPUs, SMT off, 1 NUMA node; study used 4 pinned cores (8-11)"
echo "RAM: ~30 GiB; storage: local NVMe SSD, ext4 root"
echo "kernel: $(uname -r | sed 's/-.*//') (Ubuntu); distro: $(. /etc/os-release; echo $PRETTY_NAME)"
echo "python: $(python3 -V 2>&1) (stdlib compression.zstd)"
echo "libzstd: $(python3 -c 'import compression.zstd as z; print(z.zstd_version)'); zstd CLI: $(zstd --version | sed 's/.*v//;s/,.*//')"
echo "brotli: $(python3 -c 'import brotli; print(brotli.__version__)'); xz/lzma: python stdlib lzma, liblzma $(python3 -c 'import lzma; print(lzma.LZMADecompressor.__doc__ is not None)' >/dev/null; xz --version | head -1 | sed 's/.* //')"
echo "libmagic: $(file --version | head -1)"
echo "numpy: $(python3 -c 'import numpy; print(numpy.__version__)'), scipy: $(python3 -c 'import scipy; print(scipy.__version__)')"
echo "gcc: $(gcc -dumpversion)"
