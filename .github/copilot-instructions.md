# Reviewing this repository

The scene is built on `arora-visuals` (semio-ai/arora-visuals), pinned as a git dependency in `Cargo.toml`. The controls, ghosts, outlines, picking, the instance index and the frame stages (`InstanceSystems`) all live there. arora-scene adds its own robots, values and wasm handle on top.

When a review needs that shared code, it is checked out at the exact commit `Cargo.lock` pins, in `.copilot-deps/arora-visuals`. Read it there rather than reporting the pinned implementation as unavailable.
