import { useEffect, useRef, useState, type CSSProperties } from "react";

import { mountScene, unmountScene, type SceneHandle } from "./load";
import type { Backend, Pose, SceneEvent, SceneOptions } from "./types";

/** A robot `<Scene>` shows in one instance. */
export interface SceneRobot {
  /** The robot's GLB. */
  glb: Uint8Array;
  /**
   * Names the model in `glb`: Studio's asset id, a content hash, any string
   * kept stable for the same model. A different key swaps the robot in its
   * instance once it has loaded; the old robot's joint and element controls
   * go with it. A new `glb` under the same key changes nothing, so change the
   * two together.
   */
  key: string;
  /** Where it stands when it loads, Z up, in metres; the origin if absent. */
  pose?: Pose;
}

export interface SceneProps {
  /**
   * The robots to show, by the host's own id for each instance. An id added
   * loads its robot; an id gone unloads it. A robot that cannot be loaded
   * reaches `onError`, whatever its instance showed before kept.
   */
  robots?: Record<string, SceneRobot>;
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
  robots,
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
  const first = useRef({ options, backend, onReady, onError });
  useEffect(() => {
    const el = container.current;
    if (!el) return;
    const { options, backend, onReady, onError } = first.current;
    let shown = true;
    mountScene(el, { ...options, backend }).then(
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

  // Each instance's key is asked for once: `load` itself leaves alone a key
  // that is showing or loading in its instance. An instance this `<Scene>`
  // asked for that is no longer in `robots` is unloaded. A robot that cannot
  // be read or loaded reaches `onError`, the instance's old robot kept; the
  // same key is not tried again.
  const asked = useRef(new Map<string, string>());
  useEffect(() => {
    if (!scene || !owns(scene)) return;
    const wanted = robots ?? {};
    for (const [id, robot] of Object.entries(wanted)) {
      if (asked.current.get(id) === robot.key) continue;
      asked.current.set(id, robot.key);
      scene
        .load(id, robot.glb, { key: robot.key, pose: robot.pose })
        .catch((error: unknown) =>
          onError ? onError(error) : console.error(error),
        );
    }
    for (const id of [...asked.current.keys()]) {
      if (id in wanted) continue;
      asked.current.delete(id);
      scene.unload(id);
    }
  }, [scene, robots, onError]);

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
