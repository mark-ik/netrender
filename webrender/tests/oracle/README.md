# Reference Oracle

Frozen reference PNGs for the wgpu-native rendering tests. See plan
[§6 S3](../../../wr-wgpu-notes/2026-04-28_idiomatic_wgsl_pipeline_plan.md).

Each scene is a pair: `<name>.yaml` (the wrench scene) and
`<name>.png` (the GL-rendered reference output, captured once on
upstream/0.68 + GL).

## Capture procedure

Captures run on a side worktree off `upstream/0.68`. **GL never
appears on `idiomatic-wgpu-pipeline`** — it only runs in the
ephemeral oracle-capture environment, then we bring back the PNGs.

```bash
# One-time setup (or reuse if already present):
git worktree add ../webrender-wgpu-oracle upstream/0.68
cd ../webrender-wgpu-oracle
cargo build -p wrench

# For each new seed scene:
./target/debug/wrench png \
    wrench/reftests/<category>/<scene>.yaml \
    /path/to/idiomatic-wgpu-pipeline/webrender/tests/oracle/<name>.png

# Copy the YAML alongside (so S4 can render the same scene through wgpu):
cp wrench/reftests/<category>/<scene>.yaml \
    /path/to/idiomatic-wgpu-pipeline/webrender/tests/oracle/<name>.yaml
```

The worktree's `target/` is independent of the main branch's
`target/`; switching branches in the main checkout doesn't disturb
the oracle build.

To remove the worktree when done:

```bash
git worktree remove ../webrender-wgpu-oracle
```

## Seed scene set

| Scene | Source | What it exercises |
|---|---|---|
| `blank` | `wrench/reftests/clip/blank.yaml` | empty scene; clear-colour sanity |
| `rotated_line` | `wrench/reftests/aa/rotated-line.yaml` | rotated 45° 1×200 rect; AA on a 1-px edge |
| `fractional_radii` | `wrench/reftests/aa/fractional-radii.yaml` | rounded corners with sub-pixel radii |
| `indirect_rotate` | `wrench/reftests/aa/indirect-rotate.yaml` | indirect-buffer rotation + AA |
| `linear_aligned_border_radius` | `wrench/reftests/gradient/linear-aligned-border-radius.yaml` | linear gradient on a rounded-corner shape |

Captured 2026-04-28 from `upstream/0.68` HEAD (`8c1bfc3e5`) on
NVIDIA RTX 4060 Laptop, OpenGL 3.2 / NVIDIA driver 591.86, Windows 11.

## Worktree gotcha

`upstream/0.68`'s wrench has a latent bug: `YamlFrameReader::new_from_args`
calls `args.value_of("keyframes")` / `is_present("list-resources")` /
`is_present("watch")` unconditionally, but the `png` subcommand on
that branch doesn't declare those args. Newer clap (3.x) panics on
unknown-id lookup; older clap (2.x) returned `None`/`false` silently.

Workaround on the oracle worktree only: simplify
`new_from_args` to just read `INPUT` and skip the optional
decorators. Local-only patch — never lands on
`idiomatic-wgpu-pipeline`. If we later need a fresh capture worktree,
re-apply the patch:

```rust
pub fn new_from_args(args: &clap::ArgMatches) -> YamlFrameReader {
    let yaml_file = args.value_of("INPUT").map(PathBuf::from).unwrap();
    YamlFrameReader::new(&yaml_file)
}
```

## Adding a new scene

1. Pick a `.yaml` from `wrench/reftests/` on `upstream/0.68` whose
   output is well-defined and visually simple.
2. Run `wrench png` on it (per above), saving the captured PNG here
   alongside a copy of the YAML.
3. Append a row to the seed scene table.
4. Wire it into the S4 reftest harness.

## Why the upstream/0.68 reference

`upstream/upstream` (Mozilla's gecko-dev mirror) does not ship
`wrench/reftests/`. `upstream/0.68` is the Servo packaging of
WebRender for crates.io — the closest "complete WebRender source
tree" available, and the natural baseline since our branch tracks
the same WebRender architecture. PNGs captured here are the GL-
rendered output of upstream's known-good 0.68 release plus the
five Servo packaging-prep commits.
