# Decision rules, fixed before any measurement

Written 2026-11-12, before the first compression result was seen. They are not changed after the data.
All gains are stored bytes (16 B object header included, raw fallback applied) at zstd:3, Constellation's default,
on project-disjoint test sets, with a 95 % bootstrap interval over projects.

- **R1, ship any dictionary set.** Go only if, for text types, a type-level dictionary (permissive+allowed-copyleft training)
  saves at least 8 % of stored bytes on objects of 1-64 KiB, with the interval's lower bound above 0, and the saving
  still has the same sign and at least half the size on the new-ecosystem projects.
- **R2, granularity.** A finer level is adopted over the coarser one only if it adds at least 2 percentage points (pp)
  of savings on the bytes it covers, objects < 64 KiB, with a Holm-corrected paired Wilcoxon p < 0.05
  (generic -> family -> per-type -> per-extension).
- **R3, dictionary against a higher level.** A dictionary at zstd:3 "beats raising the level" for a bucket if its saving
  is at least that of plain zstd:9 on the same objects.
- **R4, copyleft training A/B.** Copyleft-licensed training data matters for a type only if perm+copyleft beats
  permissive-only by at least 1 pp of bytes with a Holm-corrected p < 0.05. Otherwise that type's dictionary should be
  trained on permissive data only.
- **R5, selection.** A selector is adequate if its mean loss against the true-type dictionary is at most 0.5 pp of
  stored bytes and its loss against the oracle at most 1.5 pp, on objects < 64 KiB.
- **Licensing statements** are engineering assumptions for counsel to confirm, not legal conclusions.
