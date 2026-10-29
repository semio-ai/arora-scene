# arora-scene

A Semio robot scene in Bevy: the robot reading of `RobotData`, joints and their
limit-aware controls, the 3D scene around them, and the screen another renderer
draws into.

Built on [`arora-visuals`](https://github.com/semio-ai/arora-visuals), which
owns everything that is not robot-specific — the GLB extension parse, the asset
semantics, the data modelling. **The split is the point**: a capability
implemented twice has to be reconciled indefinitely, and that work produces
nothing.

## Running it

No robot ships with this repo. Point it at a Semio robot GLB of your own:

```
cargo run -- path/to/robot.glb --joint tilt_joint --ring-active --screen
```

The path is taken relative to where you run the command, and the file's own
directory becomes the asset root. That resolution happens in the binary, on
purpose: **Bevy resolves a relative asset root against the crate directory
under `cargo run` and against the executable's directory otherwise, never
against the working directory** — so a path handed straight to Bevy would mean
three different things depending on how the program was started.

`--joint list` prints the joints a robot declares and the limits they carry.
`--offscreen` is the only trustworthy timing mode: a windowed run measures the
display rather than the renderer, and `--uncapped` does nothing on macOS,
because wgpu ignores `AutoNoVsync` there.

## Building against a local `arora-visuals`

The dependency is a git URL, so an ordinary `cargo build` fetches whatever
`main` currently is. While both repos are moving together that is rarely what
you want, and the two cannot be wired by a relative path in `Cargo.toml`
without encoding where one machine keeps its checkouts.

Put the override in this repo's own `.cargo/config.toml`, which is not
tracked:

```toml
[patch."https://github.com/semio-ai/arora-visuals"]
arora-visuals = { path = "/absolute/path/to/arora-visuals/crates/arora-visuals" }
```

## The screen is a seam, not a feature

A `screen` in `RobotData` is `{width, height, resolution}` with no geometry — a
declaration. **This crate owns the surface; whatever draws the content owns the
pixels.** `mount_screen` sizes the texture from the screen's own metres and
pixels-per-metre, builds the quad under the screen's glTF node, and hands back
the `Handle<Image>`. Anything that can render into a Bevy image can fill it,
and a screen nobody fills stays blank rather than failing.

The quad is parented to the node rather than placed at the world position that
node resolves to: a screen is bolted to a robot that moves, so parenting means
every joint above it carries it for free.

## A control is meshes, not gizmo lines

Bevy offers immediate-mode gizmos for drawing lines and nothing for the
interaction. Immediate-mode gizmos also cannot be picked or shaded, so a joint
control is real meshes with their own WGSL — which is what lets the swept
range, the current value and the hover state be shader uniforms rather than
redrawn geometry. The drag is a ray cast onto the joint's plane, so it tracks
the pointer at any camera angle.

Three rules hold the model together:

- **A joint carries no `Transform`.** Its pose is `rest * f(value)`. Storing a
  transform would be a second source of truth for a derived fact, and the two
  would drift.
- **A control names its target through a relationship, not by containment.** A
  target will not always be a joint — a shape's or body's transform is the next
  case — and a control that can address more than one kind of target cannot be
  a field on any one of them.
- **Every convention is stated once, in a type that carries a basis.** Bevy's
  gizmo primitives disagree about their own plane, imported models are Y-up
  where the Studio scene is Z-up, and `Torus::new` takes inner/outer radii
  where three's `TorusGeometry` takes radius/tube. Each of these renders
  something wrong without erroring. `JointPlane` is the small version of the
  answer.

## Known gaps

- **Nothing fills the screen yet.** The Vizij face renderer is not on
  `arora-visuals`, so `mount_screen` hands back a texture no one writes. The
  seam works; the content does not arrive through it yet.
- **wasm is unbuilt and unmeasured**, and it gates every performance number
  here. `bevy_panorbit_camera` belongs to the binary rather than the library
  for this reason, but that has not been proven by building without it.
- **Assets load before the app runs.** `mount_screen` and `mount_joint` both
  read the GLB from disk up front, which is why they are the host's to call.
  Making these asset-driven is what lets a running scene gain, swap and lose
  assets — and it is the same change either way.
- **The selection outline is not built.** Bevy's transform gizmos do not cover
  it, and a mask-and-dilate pass is custom work.

## Licence

Apache-2.0, matching the source this is extracted from.
