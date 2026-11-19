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
or choose a robot GLB on the page. It plays a driver: it loads several robots,
writes values, adds joint controls and goals, writes their drags back, and
swaps and removes robots.

The workspace patches `bevy_pbr` so `StandardMaterial` draws on Safari's
WebGPU; the root `Cargo.toml` says why and when to drop it.

## The JavaScript API

```ts
import { Scene, type SceneHandle, type SceneEvent } from "@semio/scene";

<Scene
  robots={{                         // by the host's own id for each instance
    "arm-1": { glb: bytes, key: assetId },
    "arm-2": { glb: bytes, key: assetId, pose: { position: [1, 0, 0] } },
  }}
  options={{ environment: "sky" }}  // SceneOptions, read once
  onReady={(scene) => drive(scene)} // the SceneHandle
  onEvent={(event) => hear(event)}  // overlay, pick, control and load events
  style={{ height: "100%" }}        // the scene fills this element
/>;
```

`mountScene(container, options)` and `unmountScene(container)` do the same
without React, robots then loaded through the handle; `chooseBackend()` says
which backend a page would get.

### The scene's life

- **A page runs one scene for its whole life.** Bevy's windowing runs on winit,
  which allows one event loop per page and never hands it back. The package
  starts the scene once, empty, and moves its canvas into whichever `<Scene>`
  mounts: unmounting parks it, still running and drawing nothing, and mounting
  again brings the same scene back.
- **Show one `<Scene>` at a time.** A second one mounted takes the canvas, and
  from then on the first neither drives the scene nor receives its events.
- **`options` are read once**, when the page's scene first starts. A later
  `<Scene>`'s options are ignored. The theme, environment and active area can
  change at any time: `scene.setTheme`, `scene.setEnvironment` and
  `scene.setActiveArea` (the camera eases from one active area to the next;
  the first area, and the whole canvas given back, are taken at once).
- **Every robot is an instance the host names**: its own string, never
  rewritten — an Arora path, a slot name. Every call about a robot and every
  event from it carries that id. `scene.load(instance, glb, { key, pose })`
  loads a robot into an instance and `scene.unload(instance)` takes it away;
  `<Scene>` does both from its `robots` prop.
- **A robot's content is named by a key the host gives it**: Studio's asset
  id, a content hash, any string kept stable for the same model. The key,
  never the bytes, decides whether a load into an instance is a different
  robot, so the same file fetched twice is one robot. **Change a robot's bytes
  and its key together**: a key changed ahead of its bytes swaps in the old
  bytes under it.
- **A different key swaps the robot in its instance.** The new robot loads
  beside the old, which keeps showing; once it has loaded whole it takes the
  old one's place, standing where the old one stood unless the load gives a
  `pose`. The old robot goes with every control on it (joint controls, and
  transform controls on the robot or its elements). The instance's selection
  is kept, and goals stay. Other instances are untouched.
- **A robot that cannot be loaded is refused, and the instance's old one
  kept.** `load` returns a promise that resolves once the robot is showing and
  rejects with the reason it could not be read or loaded; `<Scene>` passes
  that reason to `onError`, and does not retry the same key. A load into an
  instance still loading another replaces that one, whose promise rejects.
  **The robot asked for last is the one left showing** in its instance, the
  one already showing included. `scene.robots` maps each instance to the
  robot showing in it, and `scene.loading` names the instances with a load
  under way.
- **Robot calls act in the order they were made**, within a frame too:
  `load`, `unload`, `setPose` and `setSelection`. A load then an unload of one
  instance leaves it empty, and a selection set after an unload and a load is
  the new robot's. Unloading an instance takes its selection with it, and the
  value writes and control adds still waiting for it. Control commands and
  value writes act after a frame's robot calls, whatever order they were made
  in.
- **Where a robot stands is the host's**: a `pose` on its load, or
  `scene.setPose(instance, pose)` later; the origin otherwise. A pose places
  a robot and keeps its scale. The camera
  frames every robot present when the scene first has robots to show, and
  then moves only when the user moves it or the host asks:
  `scene.frame(instances)` fits the robots named, or every robot for none.
- **`scene.setSelection(instance, elementIds)` outlines elements**, in place of
  whatever that instance outlined; none clears it.
- **The scene idles.** It draws a frame only when something changed, an empty
  scene included. WebGPU is used when the browser can give the page an
  adapter, WebGL2 otherwise; **WebGL2 draws shadows from one light only.**

### Writing values

`scene.setValue(instance, target, value, feature?)` writes `value`, one number
or three, to the robot in `instance`, in the next frame.

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
- **A write to a robot still loading waits for it**, so a driver can write as
  soon as it loads one. A write to an instance nothing is loading, or to an id
  the robot does not have, changes nothing.
- **Write as often as the values change**, every animation frame included.
  Every write and control command made in one task share one wake of the
  scene, which draws them together in its next frame.

### Controls

The scene does not decide what a control means. A driver (a device, Studio's
World, an IK solver) adds controls under keys of its own, unique in the scene
(an Arora path, say), hears each drag as a `control` event, and writes back what
the drag should change. **A control hangs off a frame in the scene**, as a
TF tree names one: a robot's root, one of its elements, or a goal of its own.

```ts
// A ring on arm-1's tilt joint, by the joint's value animatable, in its track's colour.
scene.control({
  op: "add",
  key: "arm-1/tilt",
  control: { kind: "joint", instance: "arm-1", joint: tiltValueId, color: "#22c55e" },
});
// An IK goal: a pose with nothing drawn but its handles.
scene.control({
  op: "add",
  key: "left-hand-goal",
  control: { kind: "transform", target: { kind: "goal", pose: { position: [0.3, 0, 1.2] } } },
});
scene.control({ op: "configure", key: "left-hand-goal", local: true });
scene.control({ op: "highlight", key: "left-hand-goal", on: true });
scene.control({ op: "show", key: "left-hand-goal", visible: false });
scene.control({ op: "remove", key: "left-hand-goal" });
```

- **A drag moves nothing.** It arrives as `{ type: "control", key, phase,
  value | pose, scale }` with what it asks for, and **the driver writes the
  change** through the path every other change takes: `setValue(instance,
  joint, value)` for a joint control, `control({ op: "setPose", key, pose,
  scale })` for a transform control. What the driver holds and what the scene shows cannot
  disagree. `phase` is `"start"`, `"move"` or `"end"`.
- **A transform control's target** is `{ kind: "robot", instance }`,
  `{ kind: "element", instance, id }` or `{ kind: "goal", pose }`. It offers moving,
  turning and scaling unless `modes` names fewer, along the world's axes unless
  `local` is set.
- **Colours are the host's** when it gives them, as CSS hex: `color` on a joint
  control (its joint's own otherwise), `colors` for a transform control's X, Y
  and Z (red, green and blue otherwise).
- **`configure` changes a control in place**: a joint control's `color`, a
  transform control's `colors` and `local`, and for either its `opacity` and
  whether it is `interactive`. What it leaves out stays as it is, and what a
  control's kind does not take is ignored, so one command can recolour
  controls of both kinds.
- **`opacity` fades a control** evenly to a value from 0 to 1 over a fifth of
  a second (longer while the scene draws fewer than thirty frames a second),
  so a host sends the value it wants once. Sent with the control's `add`, the
  control first shows at it, and `interactive: false` sent with it holds from
  its first frame. It multiplies any alpha the colours the host gave carry
  (`#22c55e80`); a robot's own joint colours draw opaque. It is how a control
  looks only: a faded control still takes the pointer until it is made inert.
- **`interactive: false` makes a control inert**: it takes no hover, press or
  drag, the pointer reaches the robot and the view behind it, and a drag in
  hand ends. A host's `highlight` still shows on it. The scene knows nothing
  of why: a host that plays a timeline, say, dims and stills its controls
  while it plays, and turns them back when it stops.
- **`highlight` draws a control as the one under the pointer is drawn**, until
  turned off, for a host pointing at it from elsewhere, such as its row in a
  list. `handle` names one handle of a transform control, as the overlay names
  it; the whole control when absent. The pointer's own hover works as ever.
- **A control on a robot still loading waits for it**, in order, with the
  commands sent after it; `remove` cancels anything still waiting under its
  key, and `setPose`, `configure` and `highlight` on a control added in the
  same frame apply once it exists. A control on an instance nothing is
  loading is refused.
- **`show` is the driver's.** The scene also hides controls on its own account
  (on a ghosted robot), and shows one only when neither wants it hidden.

### Events

- **`overlay`**: element and gizmo boxes on screen, and the gizmo under the
  pointer with its readings, for drawing labels and tooltips. Sent each frame
  it changes, and at once to a newly set handler.
- **`pick`**: a click on a robot, by its `instance`, with the element under
  it, or `null` for none. The scene needs the pointer's position before the press,
  which a real pointer always gives it.
- **`control`**: a drag on a control, as above.
- **`controlPress`**: a press with the right or middle button on a control's
  handle, with its `key`, `handle`, `button` and `position` in the canvas's CSS
  pixels. It is the host's to act on, to open a menu for the control, say: it
  moves nothing and turns or pans no view. A right-click over a control also
  reaches the elements around the scene as an ordinary `contextmenu` event,
  not prevented, sent from `<Scene>`'s element, so a context menu wrapped
  around `<Scene>` opens there; the browser's own menu stays shut. Anywhere
  else on the scene, a right-click pans the view and opens no menu.
- **`loaded`**: a robot loaded whole and showing in its `instance`, with its
  `key` and its `joints`: each joint's name, kind, the element it moves, the
  animatable that moves it (the `joint` a joint control names), its limits,
  and how it mimics another. It also carries `elements`, each element's id
  with the name its author gave it (`base_link`), so a host that knows a link
  by name can find its id. `scene.robots` keeps what each event said: a
  robot's event can come before a page has set its handler. `load`'s promise
  resolves from it.
- **`ready`**: a robot showing in its `instance` has been drawn as it will
  be: every mesh of it in the camera's view with its pipeline compiled (or
  failed for good, which the renderer logs), and the GPU done with that
  frame. It follows `loaded`, which says the robot is showing, not that it
  has drawn: a browser can take a while compiling its pipelines. A robot with
  nothing in view is ready at once. `drawn` is false when the scene gave up
  waiting to see it drawn, after 600 frames.
- **`loadFailed`**: a load into an `instance` that did not finish, with its
  `key` and the `error`: the GLB could not be loaded, a later load into the
  same instance replaced it, or the instance was unloaded. `load`'s promise
  rejects from it.

### Poses

**Poses are Z up**, in metres, with quaternions as `[x, y, z, w]`: the frame a
robot is described in. The scene converts to and from Bevy's Y up at the
boundary, in both directions, so a driver never sees the scene's own axes.

`scene.elementPose(instance, element)` is where an element of the robot in
`instance` stood in the last frame drawn, in that frame: what a driver needs to place a goal
relative to a link, such as an IK chain's root. It is `undefined` for an id
the robot does not declare, or before the robot has spawned.

## Embedding the Rust library

`crates/arora-scene` is the same scene for a native or editor host, and the
wasm host is the reference for wiring it.

- **Start it.** Add Bevy's `DefaultPlugins` and `ScenePlugin`, and insert
  `SceneOptions`. The scene starts empty.
- **Load robots** by pushing `robots::RobotRequest::Load`s onto
  `robots::RobotRequests::robots`, each a `robots::RobotLoad` of a
  `robots::RobotAsset { instance, key, glb, meta, joints }` (`RobotLoad::read`
  fills one from a GLB's bytes) with an optional `pose`; `glb` is an asset
  path the asset server can load. `load_from_file` does it from disk.
  Unloads, poses and selections go on the same queue, and act in the order
  they were pushed. A robot loads hidden in its instance (its root carries
  `robots::Incoming` and is not indexed or driven), and swaps in once it has
  loaded whole and spawned. How each load ended lands in
  `robots::RobotOutcomes`, in the order they ended: `Outcome::Loaded` with
  that robot's joints and elements, or `Outcome::Failed` with why, the
  instance's old robot kept.
- **Write values** by pushing `values::ValueWrite`s, each naming its
  instance, onto `values::ValueWrites` before the `values::ApplyValues` set.
  Each write may carry a `stamp`, which `values::AppliedValues` hands back once
  the write has landed.
- **Drive controls** by pushing `controls::ControlCommand`s onto
  `controls::ControlCommands`, and draining `controls::ControlEvents`. Poses
  there are the same Z-up `controls::Pose`.

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
- **Morph targets are not bound.** A robot's declaration carries none, and a
  morph animatable writes nothing.

## Licence

Apache-2.0, matching the source this is extracted from.
