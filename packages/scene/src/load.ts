import type { Backend, FeatureValue, SceneEvent, SceneOptions } from "./types";

/** The wasm module's exports this package uses. */
interface Module {
  default: (input?: unknown) => Promise<unknown>;
  mount: (canvas: string, glb: Uint8Array, options: string) => void;
  set_value: (target: string, feature: string, values: Float32Array) => void;
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
  /**
   * Writes one feature value: `target` an animatable or element id as the
   * robot's `RobotData` declares it, `feature` as `RobotData` names it
   * (`rotation`, `translation`, `color`, ...). Drawn in the next frame.
   */
  setValue(target: string, feature: string, value: FeatureValue): void;
  /** Sets where the scene's events go, replacing any earlier handler. */
  onEvent(handler: (event: SceneEvent) => void): void;
}

let mounted: Promise<SceneHandle> | undefined;

/**
 * Starts the scene in `canvas` (an element with an `id`), drawing the robot
 * in `glb`.
 *
 * **A page holds one scene, for its whole life.** The renderer's event loop
 * cannot be stopped or started twice, so a second call returns the first
 * scene rather than starting another. Hide the canvas to hide the scene.
 */
export function mountScene(
  canvas: HTMLCanvasElement,
  glb: Uint8Array,
  options: SceneOptions & { backend?: Backend } = {},
): Promise<SceneHandle> {
  mounted ??= start(canvas, glb, options);
  return mounted;
}

async function start(
  canvas: HTMLCanvasElement,
  glb: Uint8Array,
  { backend: asked, ...options }: SceneOptions & { backend?: Backend },
): Promise<SceneHandle> {
  if (!canvas.id) throw new Error("the scene's canvas needs an id");
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
    setValue(target, feature, value) {
      const values = typeof value === "number" ? [value] : value;
      wasm.set_value(target, feature, Float32Array.from(values));
    },
    onEvent(handler) {
      wasm.set_event_handler(handler);
    },
  };
}
