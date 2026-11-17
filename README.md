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

Open the example with `?glb=<url>` (and `&backend=webgl2` to force WebGL2),
or choose a robot GLB on the page. It plays a driver: it writes values, adds
joint controls and goals, writes their drags back, and swaps robots.

The workspace patches `bevy_pbr` so `StandardMaterial` draws on Safari's
WebGPU; the root `Cargo.toml` says why and when to drop it.

## The JavaScript API

```ts
import { Scene, type SceneHandle, type SceneEvent } from "@semio/scene";

<Scene
  glb={bytes}                       // the robot's GLB, as a Uint8Array
  options={{ environment: "sky" }}  // SceneOptions, read once
  onReady={(scene) => drive(scene)} // the SceneHandle
  onEvent={(event) => hear(event)}  // overlay, pick and control events
  style={{ height: "100%" }}        // the scene fills this element
/>;
```

`mountScene(container, glb, options)` and `unmountScene(container)` do the
same without React; `chooseBackend()` says which backend a page would get.

### The scene's life

- **A page runs one scene for its whole life.** Bevy's windowing runs on winit,
  which allows one event loop per page and never hands it back. The package
  starts the scene once and moves its canvas into whichever `<Scene>` mounts:
  unmounting parks it, still running and drawing nothing, and mounting again
  brings the same scene back.
- **Show one `<Scene>` at a time.** A second one mounted takes the canvas, and
  from then on the first neither drives the scene nor receives its events.
- **`options` are read once**, when the page's scene first starts. A later
  `<Scene>`'s options are ignored.
- **A different `glb` swaps the robot** in the running scene, as
  `scene.loadRobot(glb)` does. The old robot goes, with every control on it
  (joint controls, and transform controls on the robot or its elements) and
  its selection; the new one is measured and framed afresh. Goals stay. A GLB
  the scene cannot read is refused, and the old robot kept.
- **The scene idles.** It draws a frame only when something changed, and its
  camera moves only when the user moves it. WebGPU is used when the browser
  can give the page an adapter, WebGL2 otherwise; **WebGL2 draws shadows from
  one light only.**

### Writing values

`scene.setValue(target, value, feature?)` writes `value`, one number or
three, in the next frame.

- **`target` an animatable's id**, as the robot's `RobotData` declares it:
  the value lands wherever the robot binds that animatable, and an animatable
  carries its own feature, so `feature` is left out. Elements that share a
  material share its animatables, and one write changes all of them.
- **A joint's value is an animatable like any other.** It turns or slides the
  body the joint moves from the pose the GLB gave it, a mimic joint follows
  its leader scaled and offset, and **a limited joint stops at its limits**:
  a value past them poses it at the stop.
- **`target` an element's id** writes one element directly, and needs the
  `feature` (`"translation"`, `"rotation"`, `"scale"`, `"color"`,
  `"opacity"`, `"emissive"`, ...). A material write changes that element
  alone, across every primitive mesh it has, even where the GLB shares the
  material with other elements.
- **A write made before the robot has arrived waits for it**, so a driver can
  write from `onReady`. A write to an id the robot does not have changes
  nothing.

### Controls

The scene does not decide what a control means. A driver (a device, Studio's
World, an IK solver) adds controls under keys of its own, hears each drag as a
`control` event, and writes back what the drag should change.

```ts
// A ring on a joint, keyed by the joint's value animatable.
scene.control({ op: "add", key: tiltValueId, control: { kind: "joint" } });
// An IK goal: a pose with nothing drawn but its handles.
scene.control({
  op: "add",
  key: "left-hand-goal",
  control: { kind: "transform", target: { kind: "goal", pose: { position: [0.3, 0, 1.2] } } },
});
scene.control({ op: "show", key: "left-hand-goal", visible: false });
scene.control({ op: "remove", key: "left-hand-goal" });
```

- **A drag moves nothing.** It arrives as `{ type: "control", key, phase,
  value | pose, scale }` with what it asks for, and **the driver writes the
  change** through the path every other change takes: `setValue(key, value)`
  for a joint control, `control({ op: "setPose", key, pose, scale })` for a
  transform control. What the driver holds and what the scene shows cannot
  disagree. `phase` is `"start"`, `"move"` or `"end"`.
- **A transform control's target** is `{ kind: "robot" }`,
  `{ kind: "element", id }` or `{ kind: "goal", pose }`. It offers moving,
  turning and scaling unless `modes` names fewer, along the world's axes unless
  `local` is set.
- **Commands made before the robot has arrived wait for it**, in order;
  `remove` cancels anything still waiting under its key, and `setPose` on a
  goal added in the same frame applies once the goal exists.
- **`show` is the driver's.** The scene also hides controls on its own account
  (on a ghosted robot), and shows one only when neither wants it hidden.

### Events

- **`overlay`**: element and gizmo boxes on screen, and the gizmo under the
  pointer with its readings, for drawing labels and tooltips. Sent each frame
  it changes, and at once to a newly set handler.
- **`pick`**: a press released on the robot, with the element under it, or
  `null` for none. The scene needs the pointer's position before the press,
  which a real pointer always gives it.
- **`control`**: a drag on a control, as above.

### Poses

**Poses are Z up**, in metres, with quaternions as `[x, y, z, w]`: the frame a
robot is described in. The scene converts to and from Bevy's Y up at the
boundary, in both directions, so a driver never sees the scene's own axes.

## Embedding the Rust library

`crates/arora-scene` is the same scene for a native or editor host, and the
wasm host is the reference for wiring it.

- **Start it.** Add Bevy's `DefaultPlugins` and `ScenePlugin`, and insert
  `SceneOptions` and `RobotSpec { glb, copies }`, where `glb` is an asset path
  the asset server can load.
- **Declare the robot.** `mount_instances` reads the GLB from disk and
  registers `instance::index_instances`. A host holding the bytes instead
  inserts `instance::RobotMeta`, `joints::RobotJoints` and
  `instance::Selected` itself and adds `index_instances` to `Update`, as the
  wasm host does.
- **Write values** by pushing `values::ValueWrite`s onto
  `values::ValueWrites` before the `values::ApplyValues` set. Each write may
  carry a `stamp`, which `values::AppliedValues` hands back once the write has
  landed.
- **Drive controls** by pushing `controls::ControlCommand`s onto
  `controls::ControlCommands`, and draining `controls::ControlEvents`. Poses
  there are the same Z-up `controls::Pose`.
- **Swap the robot** by inserting `NextRobot { glb, meta, joints }`; the scene
  swaps it in on its next frame.

## Running it natively

No robot ships with this repo. Point it at a Semio robot GLB of your own:

```
cargo run -p arora-scene -- path/to/robot.glb --joint tilt_joint --ring-active --screen
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
- **The native tool's screen and joint mounts read the GLB up front.**
  `mount_screen` and `mount_joint` read it from disk before the app runs, so
  they describe the robot the tool started with, not one swapped in later. The
  browser package uses neither: its joint controls come from the robot being
  shown.
- **A control can be shown or hidden, not made inert.** Studio's controls can
  be visible but not draggable; these cannot yet.
- **Morph targets are not bound.** A robot's declaration carries none, and a
  morph animatable writes nothing.

## Licence

Apache-2.0, matching the source this is extracted from.
