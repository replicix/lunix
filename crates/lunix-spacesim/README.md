# lunix-spacesim

Dev-only simulator of the space gate (ADR-32). Not a dependency of the
daemon. It runs the real gate (`lunix-space`) and the real pressure rules
(`lunix-engine`) on a manual clock against a modelled workload, journal ring,
cuts, store, collector and device, so a gate or GC policy can be tried in
well under a second. Full description: `docs/plans/v1/reports/p1-spacesim.md`.

```
spacesim list
spacesim run   -s ram-64k -p main --trace out.csv
spacesim seeds -s ram-64k -p leadzone -n 8
spacesim sweep -s ram-64k --grid cache_max=12G,16G,24G --grid soft_percent=15,40 -n 5
spacesim calibrate -n 6
spacesim run -s ram-64k -p tunable --params candidate.toml
```

A parameter file is flat TOML (`key = value`, `[section]` prefixes keys; keys
starting with `policy.` go to the policy). A new policy is an `impl Policy`
in `src/policy.rs` and a line in `policy_by_name`.



