#!/bin/bash
# On the test host, from ~/dictmedia (spikes/ copied there). CPUs 12-15 only.
set -e
cd ~/dictmedia; mkdir -p corpus results logs
for g in kodak jpeg_web gnome_docs icons webp_test libtiff pdfjs libreoffice poi archive_org local_gz debs gutenberg \
         maven fdroid blender jpeg_thumb png_thumb gif; do python3 -I spikes/fetch.py $g; done   # gnu_tar omitted (stalled)
for g in kodak_derived image_derived thumbs_derived; do python3 -I spikes/fetch.py $g; done
python3 -I spikes/fetch.py blender_clips; python3 -I spikes/fetch.py audio_derived
python3 -I spikes/classify.py corpus                    # -> files.tsv (capped per format)
taskset -c 12-15 python3 -I spikes/measure.py corpus results/measure.csv.gz
python3 -I spikes/analyze.py results/measure.csv.gz results && python3 -I spikes/figs.py results
