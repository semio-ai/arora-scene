import { useEffect, useRef, useState, type CSSProperties } from "react";

import { mountScene, unmountScene, type SceneHandle } from "./load";
import type { Backend, SceneEvent, SceneOptions } from "./types";

export interface SceneProps {
  /**
   * The robot's GLB. A different one swaps the robot in the running scene; the
   * old robot's joint and element controls go with it.
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

  // A new robot swaps into the running scene; a robot the scene cannot read
  // is refused and the old one kept.
  const loaded = useRef(glb);
  useEffect(() => {
    if (!scene || glb === loaded.current) return;
    loaded.current = glb;
    try {
      scene.loadRobot(glb);
    } catch (error) {
      if (onError) onError(error);
      else console.error(error);
    }
  }, [scene, glb, onError]);

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
