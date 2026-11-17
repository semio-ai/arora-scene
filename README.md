# arora-scene

A Semio robot scene in Bevy: the robot reading of `RobotData`, joints and their
limit-aware controls, the 3D scene around them, and the screen another renderer
draws into.

Built on [`arora-visuals`](https://github.com/semio-ai/arora-visuals), which
owns everything that is not robot-specific — the GLB extension parse, the asset
semantics, the data modelling. **The split is the point**: a capability
implemented twice has to be reconciled indefinitely, and that work produces
nothing.

## Layout

| Path | What it is |
| -- | -- |
| `crates/arora-scene` | The scene as a Bevy library, with no window and no browser in it, and the native tooling behind its `cli` feature: a test window, a tuning panel, offscreen timing. |
| `crates/arora-scene-wasm` | The browser host: the wasm module the npm package loads. One build per GPU backend, because Bevy chooses its backend at compile time. |
| `packages/scene` | `@semio/scene`: the wasm modules, a typed TypeScript API, and a React `<Scene>` component. Its `example/` page drives the scene by hand. |

The npm package is the product. The crates are its source and the native
tools around it, and are not published.

## In the browser

```
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129   # must equal the wasm-bindgen crate in Cargo.lock
cd packages/scene
npm install
npm run build:wasm    # both backends into wasm/, about five minutes each from cold
npm run dev           # the example page
```

`build:wasm` also runs binaryen's `wasm-opt -Oz` when it is on `PATH` (or
`WASM_OPT` names it), which takes a fifth off the module; without it the
build still works. A wasm-bindgen CLI at any other version than the crate's
fails the build with a schema mismatch, so `WASM_BINDGEN` can point at a
specific binary.

**Using it from another project.** The package is not published yet, so
depend on this checkout: build the wasm and the package (`npm run build:wasm`
and `npm run build`), then `"@semio/scene": "file:<path>/packages/scene"`.
The package's `exports` carry a `source` condition, so a bundler set to read
it builds from `src/` rather than `dist/`. Either way the `wasm/` modules
must be built first: they are generated, not committed.

Open the example with `?glb=<url>`, or choose a robot GLB on the page. It
starts on WebGPU when the browser can give it an adapter and falls back to
WebGL2. **WebGL2 draws shadows from one light only**, so the fill light casts
none there.

**A page runs one scene for its whole life.** Bevy's windowing runs on winit,
which allows one event loop per page and never hands it back. So the package
starts the scene once and moves its canvas between the containers that show
it: unmounting `<Scene>` parks the canvas, still running and drawing nothing,
and mounting it again brings the same scene back. A different robot swaps into
the running scene (`loadRobot`, or a new `glb` on `<Scene>`): the old robot
goes, with its joint and element controls and its selection, and the new one
is measured and framed afresh. Goals stay.

The workspace patches `bevy_pbr` so `StandardMaterial` draws on Safari's
WebGPU; the root `Cargo.toml` says why and when to drop it.

## Driving it

The scene draws what it is told and reports what the user does; a driver (a
device, Studio's World, an IK solver) decides what either means.

- **Values in.** `setValue(id, value)` writes an animatable by the id
  `RobotData` gives it, and the scene moves wherever the robot binds it. A
  joint's value is an animatable like any other: it turns or slides the body
  the joint moves, from the pose the GLB gave it.
- **Controls, by the driver's keys.** `control({op: "add", key, control})`
  adds a joint control (keyed by the joint's value animatable) or a transform
  control on the robot, an element, or a goal: a pose with nothing drawn but
  the control, which is what an IK target is. `remove`, `show` and `setPose`
  take the same key.
- **A drag moves nothing.** It arrives as a `control` event with the value or
  pose it asks for, and the driver writes it back if it wants it, through the
  same `setValue` and `setPose` every other change uses. What the driver holds
  and what the scene shows cannot disagree.
- **Poses are Z up**, in metres, with quaternions as `[x, y, z, w]`: the frame
  a robot is described in. The scene converts to Bevy's Y up at the boundary.

## Running it natively

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
  something wrong without erroring.

## Known gaps

- **Nothing fills the screen yet.** The Vizij face renderer is not on
  `arora-visuals`, so `mount_screen` hands back a texture no one writes. The
  seam works; the content does not arrive through it yet.
- **Assets load before the app runs.** `mount_screen` and `mount_joint` both
  read the GLB from disk up front, which is why they are the host's to call.
  Making these asset-driven is what lets a running scene gain, swap and lose
  assets — and it is the same change either way.

## Licence

Apache-2.0, matching the source this is extracted from.
