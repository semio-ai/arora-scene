import type {
  Backend,
  BelowFloorOptions,
  ControlCommand,
  FeatureValue,
  GhostOptions,
  Modifiers,
  Pose,
  SceneEvent,
  SceneOptions,
  ShownRobot,
} from "./types";

/** The wasm module's exports this package uses. */
interface Module {
  default: (input?: unknown) => Promise<unknown>;
  mount: (canvas: string, options: string) => void;
  load: (instance: string, glb: Uint8Array, options: string) => void;
  unload: (instance: string) => void;
  set_pose: (instance: string, pose: string) => void;
  set_selection: (instance: string, elements: string[]) => void;
  set_ghost: (instance: string, options: string) => void;
  frame: (instances: string[]) => void;
  set_theme: (theme: string) => void;
  set_environment: (environment: string) => void;
  set_below_floor: (options: string) => void;
  set_active_area: (area: Float32Array) => void;
  set_value: (
    instance: string,
    target: string,
    feature: string,
    values: Float32Array,
  ) => void;
  control: (command: string) => void;
  cancel_drag: () => void;
  set_modifiers: (
    alt: boolean,
    shift: boolean,
    meta: boolean,
    ctrl: boolean,
  ) => void;
  element_pose: (instance: string, element: string) => Pose | undefined;
  set_event_handler: (handler: (event: SceneEvent) => void) => void;
}

// One import per backend, each a separate chunk, so a page downloads only
// the module it runs.
const modules: Record<Backend, () => Promise<Module>> = {
  webgpu: () => import("../wasm/webgpu/arora_scene_wasm.js") as Promise<Module>,
  webgl2: () => import("../wasm/webgl2/arora_scene_wasm.js") as Promise<Module>,
};

/**
 * WebGPU when the browser can give it an adapter, WebGL2 otherwise. An
 * exposed `navigator.gpu` is not enough: a browser can expose the API and
 * still have no adapter for this device.
 */
export async function chooseBackend(): Promise<Backend> {
  const gpu = (
    navigator as Navigator & { gpu?: { requestAdapter(): Promise<unknown> } }
  ).gpu;
  if (!gpu) return "webgl2";
  try {
    return (await gpu.requestAdapter()) ? "webgpu" : "webgl2";
  } catch {
    return "webgl2";
  }
}

/** How a robot is loaded into an instance. */
export interface LoadOptions {
  /**
   * Names the model: Studio's asset id, a content hash, any string the host
   * keeps stable for the same model. Loading a key an instance already shows,
   * with none loading, or the key it is loading, changes nothing.
   */
  key?: string;
  /**
   * Where the robot stands, Z up, in metres. Absent, it stands where the
   * robot it replaces stood, or at the origin.
   */
  pose?: Pose;
}

/**
 * A running scene. **Every robot in it is an instance the host names**: its
 * own string, never rewritten, which every call about that robot and every
 * event from it carries.
 */
export interface SceneHandle {
  readonly backend: Backend;
  /** The canvas the scene draws into, wherever it is currently placed. */
  readonly canvas: HTMLCanvasElement;
  /**
   * Loads the robot in `glb` into the instance `instance`. Under a new id it
   * adds a robot; under one the scene shows it swaps that robot, which keeps
   * showing until the new one has loaded whole, and then goes with the
   * controls on its joints and elements. Resolves once the robot is showing.
   * Rejects, keeping what the instance showed, if `glb` is not a robot the
   * scene can read, fails to load, or a later load into the same instance
   * replaces it first.
   */
  load(instance: string, glb: Uint8Array, options?: LoadOptions): Promise<void>;
  /**
   * Takes the robot in `instance` away, with the controls on it and anything
   * still loading into it, whose promise rejects.
   */
  unload(instance: string): void;
  /** Moves the robot in `instance`: Z up, in metres. */
  setPose(instance: string, pose: Pose): void;
  /**
   * Outlines `elements` of the robot in `instance`, by their `RobotData` ids,
   * in place of whatever it outlined; none clears it. Kept across a swap.
   */
  setSelection(instance: string, elements: string[]): void;
  /**
   * Draws the robot in `instance` as a ghost, styled by `ghost`, or takes
   * the page's ghost off for `null`: its style goes, and the ghost too unless
   * the robot was one before the page asked. Kept across a swap; called again with a new style, the
   * ghost changes from the next frame, so a page fades one by calling it with
   * a falling `opacity`.
   */
  setGhost(instance: string, ghost: GhostOptions | null): void;
  /**
   * Writes a value to the robot in `instance`.
   *
   * With a `feature`, `target` is an element's id and `feature` the feature
   * to change, as `RobotData` names it: `translation`, `rotation` (an euler,
   * radians), `scale` (three, or one scaling evenly), `color`, `emissive`
   * (linear RGB), `opacity`, `metalness`, `emissiveIntensity` (one), one
   * component of a triple by its per-axis name (`translation.x`,
   * `rotation.r`, `color.g`: one), or one of the element's morph target
   * names (a weight). These are the names, shapes and meaning
   * `@vizij/runtime`'s `setValue` takes for a face's elements.
   * `emissiveIntensity` scales the element's `emissive` colour, each kept on
   * its own, so writing either leaves the other as it was.
   *
   * Without one, `target` is an animatable's id as the robot's `RobotData`
   * declares it, which carries its own feature (a joint's value among them).
   * A face has no such form: its animatables are its device's outputs, which
   * the device writes. A robot's have no device behind them in the scene, so
   * the page writes them here.
   *
   * A feature the element lacks, or a value of the wrong shape for it,
   * changes nothing. Drawn in the next frame; for a robot still loading, in
   * the frame it arrives in.
   */
  setValue(
    instance: string,
    target: string,
    value: FeatureValue,
    feature?: string,
  ): void;
  /**
   * Adds, removes, shows, hides or moves a control, by the driver's key for
   * it, unique in the scene. A drag on a control moves nothing: it arrives as
   * a `control` event, and the driver writes the change.
   */
  control(command: ControlCommand): void;
  /**
   * Abandons the drag in hand on a control, if there is one: it closes with a
   * `cancel` event, and nothing more is sent for it, the button coming up
   * included. Call it for what the scene cannot see itself: Escape, the
   * window losing focus, a `pointercancel`, the page being hidden. Takes
   * effect in the next frame, before anything else happens to the drag.
   */
  cancelDrag(): void;
  /**
   * Says which modifier keys are held, for the scene to stamp on each
   * `control` event from the next frame on, until called again. Pass the
   * state from the page's own pointer events (`altKey`, `shiftKey`,
   * `metaKey`, `ctrlKey`), and its key events where a key changes mid-drag:
   * the canvas sees no keys while it lacks focus. Every key not held is
   * `false`; none is held until this is first called.
   */
  setModifiers(modifiers: Modifiers): void;
  /**
   * The robots showing, by instance, as their `loaded` events described
   * them. A robot's event can come before a page has set its handler, so
   * read this rather than rely on hearing it.
   */
  readonly robots: ReadonlyMap<string, ShownRobot>;
  /** The instances with a load under way, showing a robot yet or not. */
  readonly loading: ReadonlySet<string>;
  /**
   * Where an element of the robot in `instance` stood in the last frame
   * drawn, by its `RobotData` id: Z up, in metres, the rotation a quaternion
   * `[x, y, z, w]`, the frame goal poses are given in. `undefined` for an id
   * the robot does not declare, or before it has spawned.
   */
  elementPose(instance: string, element: string): Pose | undefined;
  /**
   * Moves the camera to fit the robots in `instances`, or every robot when
   * none are named. The camera fits the robots present when the scene first
   * has robots by itself; robots loaded after that are framed only when
   * asked.
   */
  frame(instances?: string[]): void;
  setTheme(theme: NonNullable<SceneOptions["theme"]>): void;
  setEnvironment(environment: NonNullable<SceneOptions["environment"]>): void;
  /**
   * Shows what has sunk below the floor through it, or stops showing it for
   * `undefined`.
   */
  setBelowFloor(options: BelowFloorOptions | undefined): void;
  /**
   * The part of the canvas left visible beside the host's panels,
   * `[x, y, width, height]` in CSS pixels, which the camera frames the robots
   * in, easing there from an area set before; the first is taken at once,
   * and `undefined` gives the whole canvas back at once.
   */
  setActiveArea(area: SceneOptions["activeArea"]): void;
  /** Sets where the scene's events go, replacing any earlier handler. */
  onEvent(handler: (event: SceneEvent) => void): void;
}

/**
 * A load waiting for its outcome. One whose instance was unloaded is already
 * settled, and stays only to take its own outcome when that arrives.
 */
interface Pending {
  key?: string;
  resolve: () => void;
  reject: (error: Error) => void;
  /** Its instance was unloaded after it was asked for. */
  unloaded?: true;
}

/** What the package knows of one instance. */
interface Instance {
  /** The robot showing, once one is. */
  robot?: ShownRobot;
  /** Loads asked for and not yet settled, oldest first. */
  pending: Pending[];
  /** The load asked for last and not yet settled, so a repeat joins it. */
  loading?: { key?: string; done: Promise<void> };
}

/**
 * The page's one scene. The renderer's event loop runs for the life of the
 * page and cannot be started twice, so the scene is started once and its
 * canvas moves between whichever containers show it.
 */
let running:
  | {
      canvas: HTMLCanvasElement;
      handle: Promise<SceneHandle>;
    }
  | undefined;

/**
 * Where the canvas waits while no container shows it. It stays in the
 * document so the renderer still finds it, and draws nothing there: a canvas
 * with no size has no frames to draw.
 */
function parking(): HTMLElement {
  const id = "semio-scene-parking";
  let el = document.getElementById(id);
  if (!el) {
    el = document.createElement("div");
    el.id = id;
    el.hidden = true;
    document.body.append(el);
  }
  return el;
}

/**
 * Shows the page's scene in `container`, starting it, empty, with `options`
 * if it is not running; later calls move the running scene into their
 * container, and their `options` are ignored. Robots are loaded through the
 * handle.
 */
export function mountScene(
  container: HTMLElement,
  options: SceneOptions & { backend?: Backend } = {},
): Promise<SceneHandle> {
  if (!running) {
    const canvas = document.createElement("canvas");
    canvas.id = "semio-scene";
    canvas.style.cssText =
      "display:block;width:100%;height:100%;outline:none;touch-action:none";
    container.append(canvas);
    const handle = start(canvas, options);
    running = { canvas, handle };
    // A start that fails (a module that did not download, options the scene
    // refused) never reached the event loop, so the next call may try again.
    handle.catch(() => {
      if (running?.handle !== handle) return;
      canvas.remove();
      running = undefined;
    });
  } else {
    container.append(running.canvas);
  }
  return running.handle;
}

/**
 * Takes the scene out of `container`, if it is there, and parks it until a
 * container shows it again.
 */
export function unmountScene(container: HTMLElement): void {
  if (running && running.canvas.parentElement === container)
    parking().append(running.canvas);
}

async function start(
  canvas: HTMLCanvasElement,
  { backend: asked, ...options }: SceneOptions & { backend?: Backend },
): Promise<SceneHandle> {
  const backend = asked ?? (await chooseBackend());
  const wasm = await modules[backend]();
  await wasm.default();
  try {
    wasm.mount(`#${CSS.escape(canvas.id)}`, JSON.stringify(options));
  } catch (error) {
    // winit hands the event loop to the browser by throwing, on purpose, once
    // the scene is running. Anything else is a real failure.
    if (!String(error).includes("Using exceptions for control flow"))
      throw error;
  }
  const instances = new Map<string, Instance>();
  const robots = new Map<string, ShownRobot>();
  const instance = (id: string): Instance => {
    let held = instances.get(id);
    if (!held) {
      held = { pending: [] };
      instances.set(id, held);
    }
    return held;
  };
  // Whether the pointer is over a control, from the overlay, and whether the
  // last press other than the primary's was (see the listeners below).
  let overControl = false;
  let pressedOnControl = false;
  // Every handler the page sets goes through this, so each load is settled
  // whether or not the page listens. An instance's outcomes arrive in the
  // order its loads were asked for, so each settles the oldest still waiting.
  const dispatch =
    (handler?: (event: SceneEvent) => void) => (event: SceneEvent) => {
      if (event.type === "overlay")
        overControl = event.pointer !== null && !event.pointer.held;
      if (event.type === "loaded" || event.type === "loadFailed") {
        const held = instance(event.instance);
        const settled = held.pending.shift();
        // A load whose instance was unloaded since is answered already, and
        // what it loaded is going: the scene takes it away next frame.
        if (settled?.unloaded) {
          // Nothing to settle or show.
        } else if (event.type === "loaded") {
          const { type: _, ...robot } = event;
          // Set before the handler hears of it, so the handle already says
          // which robot is showing when it does.
          held.robot = robot;
          robots.set(event.instance, robot);
          settled?.resolve();
        } else settled?.reject(new Error(event.error));
        // With nothing it asked for left unsettled, the instance is no longer
        // loading — said before the handler hears of it, as the robot is.
        if (!held.pending.some((waiting) => !waiting.unloaded))
          held.loading = undefined;
      }
      handler?.(event);
    };
  wasm.set_event_handler(dispatch());
  // A right-click over a control is the host's, to open its own menu there:
  // the renderer prevents every context menu on its canvas, and a menu
  // component skips an event already prevented. So one over a control is
  // prevented here, which keeps the browser's own menu shut, kept from the
  // renderer, and sent on as a fresh, unprevented copy from the canvas's
  // container, where a menu wrapped around the scene hears it.
  //
  // Whether it is over a control is read from the overlay at the press,
  // before the scene has seen the press, since a browser may fire the menu
  // event only once the button is released. That reading lasts for the one
  // press: the next press replaces it, and it lapses once the button is up,
  // so a menu opened from the keyboard later is never taken for it.
  canvas.addEventListener(
    "pointerdown",
    (e) => {
      pressedOnControl = e.button !== 0 && overControl;
    },
    { capture: true },
  );
  canvas.addEventListener(
    "pointerup",
    () => {
      setTimeout(() => (pressedOnControl = false));
    },
    { capture: true },
  );
  canvas.addEventListener(
    "contextmenu",
    (e) => {
      if (!e.isTrusted || !pressedOnControl) return;
      pressedOnControl = false;
      e.preventDefault();
      e.stopImmediatePropagation();
      canvas.parentElement?.dispatchEvent(new MouseEvent("contextmenu", e));
    },
    { capture: true },
  );
  return {
    backend,
    canvas,
    robots,
    get loading() {
      const ids = new Set<string>();
      for (const [id, held] of instances) if (held.loading) ids.add(id);
      return ids;
    },
    elementPose(id, element) {
      return wasm.element_pose(id, element);
    },
    setValue(id, target, value, feature) {
      const values = typeof value === "number" ? [value] : value;
      wasm.set_value(id, target, feature ?? "", Float32Array.from(values));
    },
    control(command) {
      wasm.control(JSON.stringify(command));
    },
    cancelDrag() {
      wasm.cancel_drag();
    },
    setModifiers({ alt, shift, meta, ctrl }) {
      wasm.set_modifiers(alt, shift, meta, ctrl);
    },
    setPose(id, pose) {
      wasm.set_pose(id, JSON.stringify(pose));
    },
    setSelection(id, elements) {
      wasm.set_selection(id, elements);
    },
    setGhost(id, ghost) {
      wasm.set_ghost(id, ghost ? JSON.stringify(ghost) : "");
    },
    frame(ids = []) {
      wasm.frame(ids);
    },
    setTheme(theme) {
      wasm.set_theme(theme);
    },
    setEnvironment(environment) {
      wasm.set_environment(environment);
    },
    setBelowFloor(options) {
      wasm.set_below_floor(options ? JSON.stringify(options) : "");
    },
    setActiveArea(area) {
      wasm.set_active_area(Float32Array.from(area ?? []));
    },
    unload(id) {
      robots.delete(id);
      wasm.unload(id);
      const held = instances.get(id);
      if (!held) return;
      held.robot = undefined;
      held.loading = undefined;
      // Anything still loading into it is settled now; the scene answers each
      // with a `loadFailed` later. Each stays queued, settled, to take that
      // answer, so a load asked for after this one settles on its own.
      for (const waiting of held.pending) {
        if (waiting.unloaded) continue;
        waiting.reject(new Error("its instance was unloaded"));
        waiting.unloaded = true;
      }
    },
    load(id, glb, { key, pose } = {}) {
      const held = instance(id);
      // Compared with the robot last asked for, not only the one showing: a
      // request for the robot showing while another loads must replace that
      // one, or the robot asked for last would not be the one left showing.
      if (key !== undefined) {
        if (held.loading) {
          if (held.loading.key === key) return held.loading.done;
        } else if (held.robot?.key === key) {
          return Promise.resolve();
        }
      }
      try {
        wasm.load(id, glb, JSON.stringify({ key, pose }));
      } catch (error) {
        return Promise.reject(error);
      }
      const settled = new Promise<void>((resolve, reject) =>
        held.pending.push({ key, resolve, reject }),
      );
      const loading = {
        key,
        done: settled.finally(() => {
          if (held.loading === loading) held.loading = undefined;
        }),
      };
      held.loading = loading;
      return loading.done;
    },
    onEvent(handler) {
      wasm.set_event_handler(dispatch(handler));
    },
  };
}
