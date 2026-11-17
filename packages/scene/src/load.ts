import type {
  Backend,
  ControlCommand,
  FeatureValue,
  SceneEvent,
  SceneOptions,
} from "./types";

/** The wasm module's exports this package uses. */
interface Module {
  default: (input?: unknown) => Promise<unknown>;
  mount: (canvas: string, glb: Uint8Array, options: string) => void;
  set_value: (target: string, feature: string, values: Float32Array) => void;
  control: (command: string) => void;
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
  /** Sets where the scene's events go, replacing any earlier handler. */
  onEvent(handler: (event: SceneEvent) => void): void;
}

/**
 * The page's one scene. The renderer's event loop runs for the life of the
 * page and cannot be started twice, so the scene is started once and its
 * canvas moves between whichever containers show it.
 */
let running:
  | { canvas: HTMLCanvasElement; glb: Uint8Array; handle: Promise<SceneHandle> }
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
 * Shows the page's scene in `container`, starting it with `glb` and `options`
 * the first time. Later calls move the running scene into their container,
 * and their `glb` and `options` are ignored: the scene keeps drawing the
 * robot it started with.
 */
export function mountScene(
  container: HTMLElement,
  glb: Uint8Array,
  options: SceneOptions & { backend?: Backend } = {},
): Promise<SceneHandle> {
  if (!running) {
    const canvas = document.createElement("canvas");
    canvas.id = "semio-scene";
    canvas.style.cssText =
      "display:block;width:100%;height:100%;outline:none;touch-action:none";
    container.append(canvas);
    const handle = start(canvas, glb, options);
    running = { canvas, glb, handle };
    // A start that fails (a module that did not download, a robot the scene
    // refused) never reached the event loop, so the next call may try again.
    handle.catch(() => {
      if (running?.handle !== handle) return;
      canvas.remove();
      running = undefined;
    });
  } else {
    if (glb !== running.glb)
      console.warn(
        "@semio/scene: the scene keeps the robot it started with; a different robot was ignored",
      );
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
  return {
    backend,
    canvas,
    setValue(target, value, feature) {
      const values = typeof value === "number" ? [value] : value;
      wasm.set_value(target, feature ?? "", Float32Array.from(values));
    },
    control(command) {
      wasm.control(JSON.stringify(command));
    },
    onEvent(handler) {
      wasm.set_event_handler(handler);
    },
  };
}
