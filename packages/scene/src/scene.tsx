import { useEffect, useRef, useState, type CSSProperties } from "react";

import { showScene, unmountScene, type SceneHandle } from "./load";
import type { Backend, SceneEvent, SceneOptions } from "./types";

export interface SceneProps {
  /**
   * The robot's GLB. A different one swaps the robot in the running scene
   * once it has loaded; the old robot's joint and element controls go with
   * it. One that cannot be loaded reaches `onError`, the old robot kept.
   */
  glb: Uint8Array;
  /** How the scene starts. Read once, when the page's scene starts. */
  options?: SceneOptions;
  /** Force a backend instead of choosing by what the browser supports. */
  backend?: Backend;
  /** Called with the running scene, to drive it. */
  onReady?: (scene: SceneHandle) => void;
  onEvent?: (event: SceneEvent) => void;
  onError?: (error: unknown) => void;
  className?: string;
  style?: CSSProperties;
}

/**
 * The robot scene, filling this element. Give the element a size.
 *
 * A page has one scene, which runs for the life of the page. `<Scene>` shows
 * it: mounting moves the scene into this element, and unmounting parks it,
 * still running and drawing nothing, until a `<Scene>` mounts again. Show one
 * `<Scene>` at a time; a second takes the scene from the first.
 */
export function Scene({
  glb,
  options,
  backend,
  onReady,
  onEvent,
  onError,
  className,
  style,
}: SceneProps) {
  const container = useRef<HTMLDivElement>(null);
  const [scene, setScene] = useState<SceneHandle>();

  // Read at mount: a running scene does not restart for new props.
  const first = useRef({ glb, options, backend, onReady, onError });
  useEffect(() => {
    const el = container.current;
    if (!el) return;
    const { glb, options, backend, onReady, onError } = first.current;
    let shown = true;
    // Shown with the robot it already has: a different `glb` is swapped in
    // below, where a robot that fails to load reaches `onError` and the
    // scene, still showing the old robot, stays drivable.
    showScene(el, glb, { ...options, backend }).then(
      (handle) => {
        if (!shown) return;
        setScene(handle);
        onReady?.(handle);
      },
      (error: unknown) => (onError ? onError(error) : console.error(error)),
    );
    return () => {
      shown = false;
      unmountScene(el);
    };
  }, []);

  // Whether this `<Scene>` is the one showing the page's scene. Another one
  // mounted later takes the canvas, and from then on this one neither drives
  // the scene nor hears from it.
  const owns = (handle: SceneHandle) =>
    container.current !== null &&
    handle.canvas.parentElement === container.current;

  // A `glb` other than the robot showing swaps into the running scene. A
  // robot that cannot be read or loaded reaches `onError`, the old one kept;
  // the same bytes are not tried again, but a new array is.
  const asked = useRef<Uint8Array>(undefined);
  useEffect(() => {
    if (!scene || glb === scene.glb || glb === asked.current || !owns(scene))
      return;
    asked.current = glb;
    scene
      .loadRobot(glb)
      .catch((error: unknown) =>
        onError ? onError(error) : console.error(error),
      );
  }, [scene, glb, onError]);

  // Events go to the `<Scene>` showing the scene, and only while it does: a
  // parked scene, or one another `<Scene>` has taken, must not call into this
  // one, and this one's cleanup must not clear the other's handler.
  useEffect(() => {
    if (!scene || !onEvent || !owns(scene)) return;
    let live = true;
    scene.onEvent((event) => {
      if (live && owns(scene)) onEvent(event);
    });
    return () => {
      live = false;
      if (owns(scene)) scene.onEvent(() => {});
    };
  }, [scene, onEvent]);

  return (
    <div
      ref={container}
      className={className}
      style={{ position: "relative", ...style }}
    />
  );
}
