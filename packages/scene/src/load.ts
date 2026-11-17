import type {
  Backend,
  ControlCommand,
  FeatureValue,
  Pose,
  SceneEvent,
  SceneOptions,
} from "./types";

/** The wasm module's exports this package uses. */
interface Module {
  default: (input?: unknown) => Promise<unknown>;
  mount: (canvas: string, glb: Uint8Array, options: string) => void;
  set_value: (target: string, feature: string, values: Float32Array) => void;
  control: (command: string) => void;
  load_robot: (glb: Uint8Array) => string;
  first_robot: () => string;
  element_pose: (element: string) => Pose | undefined;
  set_event_handler: (handler: (event: WasmEvent) => void) => void;
}

/** An event as the module sends it: a robot named by its swap id. */
type WasmEvent =
  | Exclude<SceneEvent, { type: "robot" }>
  | (Omit<Extract<SceneEvent, { type: "robot" }>, "key"> & { glb: string });

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

/** A running scene. */
export interface SceneHandle {
  readonly backend: Backend;
  /** The canvas the scene draws into, wherever it is currently placed. */
  readonly canvas: HTMLCanvasElement;
  /**
   * Writes a value. `target` is an animatable's id as the robot's `RobotData`
   * declares it, which carries its own feature (a joint's value among them),
   * or an element's id together with the `feature` to change, as `RobotData`
   * names it (`rotation`, `translation`, `color`, ...). Drawn in the next
   * frame; before the robot has spawned, in the frame it arrives in.
   */
  setValue(target: string, value: FeatureValue, feature?: string): void;
  /**
   * Adds, removes, shows, hides or moves a control, by the driver's key for
   * it. A drag on a control moves nothing: it arrives as a `control` event,
   * and the driver writes the change.
   */
  control(command: ControlCommand): void;
  /**
   * Replaces the robot with the one in `glb`, in the same running scene.
   * `key` names the model, and is what decides whether it is a different
   * one: Studio's asset id, a content hash, any string the host keeps stable
   * for the same model. The new robot loads beside the old, which keeps
   * showing until it is replaced. Resolves once the new robot is showing;
   * the old robot's joint and element controls have gone with it, and goals
   * stay. Rejects, keeping the old robot, if `glb` is not a robot the scene
   * can read or fails to load.
   *
   * The robot asked for last is the one left showing. A later call replaces
   * a robot still loading, whose promise rejects; that holds for the robot
   * showing too, so asking for it again while another loads keeps it. Asking
   * for the key showing, with none loading, or for the key loading, changes
   * nothing.
   */
  loadRobot(glb: Uint8Array, key: string): Promise<void>;
  /** The key of the robot showing. */
  readonly key: string;
  /**
   * Where an element of the robot showing stood in the last frame drawn, by
   * its `RobotData` id: Z up, in metres, the rotation a quaternion
   * `[x, y, z, w]`, the frame goal poses are given in. `undefined` for an id
   * the robot does not declare, or before it has spawned.
   */
  elementPose(element: string): Pose | undefined;
  /** Sets where the scene's events go, replacing any earlier handler. */
  onEvent(handler: (event: SceneEvent) => void): void;
}

/**
 * The page's one scene. The renderer's event loop runs for the life of the
 * page and cannot be started twice, so the scene is started once and its
 * canvas moves between whichever containers show it.
 */
let running:
  | {
      canvas: HTMLCanvasElement;
      /** The key of the robot showing: changed only once a swap has finished. */
      key: string;
      handle: Promise<SceneHandle>;
      /** A swap under way, so a second request for it joins the first. */
      loading?: { key: string; done: Promise<void> };
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
 * Shows the page's scene in `container`, starting it with the robot in `glb`,
 * named by `key`, and `options` the first time. Later calls move the running
 * scene into their container and swap in their robot if `key` names a
 * different one (see `loadRobot`), resolving once it is showing and
 * rejecting if it cannot be; their `options` are ignored, the scene having
 * started with the first call's.
 */
export function mountScene(
  container: HTMLElement,
  glb: Uint8Array,
  key: string,
  options: SceneOptions & { backend?: Backend } = {},
): Promise<SceneHandle> {
  return showScene(container, glb, key, options).then(async (scene) => {
    await scene.loadRobot(glb, key);
    return scene;
  });
}

/**
 * Shows the page's scene in `container`, starting it with the robot in `glb`,
 * named by `key`, and `options` if it is not running, and otherwise leaving
 * its robot as it is.
 */
export function showScene(
  container: HTMLElement,
  glb: Uint8Array,
  key: string,
  options: SceneOptions & { backend?: Backend } = {},
): Promise<SceneHandle> {
  if (!running) {
    const canvas = document.createElement("canvas");
    canvas.id = "semio-scene";
    canvas.style.cssText =
      "display:block;width:100%;height:100%;outline:none;touch-action:none";
    container.append(canvas);
    const handle = start(canvas, glb, key, options);
    running = { canvas, key, handle };
    // A start that fails (a module that did not download, a robot the scene
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
  glb: Uint8Array,
  key: string,
  { backend: asked, ...options }: SceneOptions & { backend?: Backend },
): Promise<SceneHandle> {
  const backend = asked ?? (await chooseBackend());
  const wasm = await modules[backend]();
  await wasm.default();
  try {
    wasm.mount(`#${CSS.escape(canvas.id)}`, glb, JSON.stringify(options));
  } catch (error) {
    // winit hands the event loop to the browser by throwing, on purpose, once
    // the scene is running. Anything else is a real failure.
    if (!String(error).includes("Using exceptions for control flow"))
      throw error;
  }
  // Robots given to the scene and not yet settled, by the id the module
  // names each one by in its `robot` event, with the host's key for it. Every
  // handler the page sets goes through `dispatch`, so a swap is settled
  // whether or not the page listens, and the page hears of robots by its own
  // keys.
  const robots = new Map<
    string,
    { key: string; resolve?: () => void; reject?: (error: Error) => void }
  >([[wasm.first_robot(), { key }]]);
  const dispatch =
    (handler?: (event: SceneEvent) => void) => (event: WasmEvent) => {
      if (event.type !== "robot") {
        handler?.(event);
        return;
      }
      const { glb, ...outcome } = event;
      const robot = robots.get(glb);
      robots.delete(glb);
      if (!robot) return;
      if (outcome.error === undefined) robot.resolve?.();
      else robot.reject?.(new Error(outcome.error));
      handler?.({ ...outcome, key: robot.key });
    };
  wasm.set_event_handler(dispatch());
  return {
    backend,
    canvas,
    get key() {
      return running?.key ?? key;
    },
    elementPose(element) {
      return wasm.element_pose(element);
    },
    setValue(target, value, feature) {
      const values = typeof value === "number" ? [value] : value;
      wasm.set_value(target, feature ?? "", Float32Array.from(values));
    },
    control(command) {
      wasm.control(JSON.stringify(command));
    },
    loadRobot(next, nextKey) {
      // Compared with the robot last asked for, not only the one showing: a
      // request for the robot showing while another loads must replace that
      // one, or the robot asked for last would not be the one left showing.
      if (running?.loading) {
        if (running.loading.key === nextKey) return running.loading.done;
      } else if (nextKey === (running?.key ?? key)) {
        return Promise.resolve();
      }
      let id: string;
      try {
        id = wasm.load_robot(next);
      } catch (error) {
        return Promise.reject(error);
      }
      const settled = new Promise<void>((resolve, reject) =>
        robots.set(id, { key: nextKey, resolve, reject }),
      );
      const loading = {
        key: nextKey,
        // The robot showing changes only once the swap has finished, so a
        // robot that failed is never taken for the one showing.
        done: settled.then(
          () => {
            if (running) running.key = nextKey;
            if (running?.loading === loading) running.loading = undefined;
          },
          (error: unknown) => {
            if (running?.loading === loading) running.loading = undefined;
            throw error;
          },
        ),
      };
      if (running) running.loading = loading;
      return loading.done;
    },
    onEvent(handler) {
      wasm.set_event_handler(dispatch(handler));
    },
  };
}
