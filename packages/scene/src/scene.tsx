import { useEffect, useRef, useState, type CSSProperties } from "react";

import { mountScene, unmountScene, type SceneHandle } from "./load";
import type { Backend, SceneEvent, SceneOptions } from "./types";

export interface SceneProps {
  /**
   * The robot's GLB. The page's scene draws the robot it started with, so
   * only the first `<Scene>` on a page decides it.
   */
  glb: Uint8Array;
  /** How the scene starts. Read when the page's scene starts, as `glb` is. */
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
    mountScene(el, glb, { ...options, backend }).then(
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

  // Events go to the shown `<Scene>` only: a parked scene must not call into
  // a component that is gone.
  useEffect(() => {
    if (!scene || !onEvent) return;
    scene.onEvent(onEvent);
    return () => scene.onEvent(() => {});
  }, [scene, onEvent]);

  return (
    <div
      ref={container}
      className={className}
      style={{ position: "relative", ...style }}
    />
  );
}
