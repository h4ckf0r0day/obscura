# 2026-09-11 classic WPT baseline

Full classic testharness run against:

- Obscura `01e1caa33360f6c02643457307894ec885e82eef`
  (`obscura 0.1.0-dev+01e1caa3`, `--no-default-features`)
- WPT `03f14d4780c4d981bc84c65679b18e9327a1affe`
- 32 workers and a 15-second settle wait

Outcome:

- Files/variants: 38,532
- Supported: 35,040; unsupported runner automation: 3,492
- Passing files: 3,555
- Passing subtests: 603,997 / 893,328 (67.6%)
- Runtime: 15,436,553 ms (4h 17m 16s)
- Core: 327,094 / 392,547 (83.3%)
- Relevant: 521,393 / 600,709 (86.8%)

`results.json.gz` is the complete JSON output. Decompress it with
`gzip -dc results.json.gz > results.json`. `triage.md` is its human-readable
failure summary.

SHA-256 checksums:

```text
fcc1f2a9b1db83105f06a42220d29d15f881873852c31c8982efaae7b684d769  results.json (uncompressed)
c7d36069280e3425ccca60c46c6c8f6c37acd89dd209b81aa31ef27d2638ea78  results.json.gz
714deee8244b50c841f886e4ac52af3cdc4c0ceb20e39d29a2a39dd3f25cbb1a  triage.md
```
