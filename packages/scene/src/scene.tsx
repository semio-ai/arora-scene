import { useEffect, useId, useRef, useState, type CSSProperties } from "react";

import { mountScene, type SceneHandle } from "./load";
import type { Backend, SceneEvent, SceneOptions } from "./types";

export interface SceneProps {
  /** The robot's GLB. Read once: the scene draws the first one it is given. */
  glb: Uint8Array;
  /** How the scene starts. Read once, at mount. */
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
 * The robot scene, filling its container. The scene sizes itself to the
 * element around its canvas, so give that element a size.
 *
 * **Mount it once per page.** The renderer cannot be stopped and started
 * again, so a second `<Scene>` or a remount drives the scene the first one
 * started. Hide the component rather than unmounting it.
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
  const canvas = useRef<HTMLCanvasElement>(null);
  const id = `semio-scene-${useId().replaceAll(":", "")}`;
  const [scene, setScene] = useState<SceneHandle>();

  // The props a mount reads are read once, so this effect runs once.
  const first = useRef({ glb, options, backend, onReady, onError });
  useEffect(() => {
    const { glb, options, backend, onReady, onError } = first.current;
    if (!canvas.current) return;
    mountScene(canvas.current, glb, { ...options, backend }).then(
      (handle) => {
        setScene(handle);
        onReady?.(handle);
      },
      (error: unknown) => (onError ? onError(error) : console.error(error)),
    );
  }, []);

  useEffect(() => {
    if (scene && onEvent) scene.onEvent(onEvent);
  }, [scene, onEvent]);

  return (
    <div className={className} style={{ position: "relative", ...style }}>
      <canvas
        ref={canvas}
        id={id}
        style={{
          display: "block",
          width: "100%",
          height: "100%",
          outline: "none",
        }}
      />
    </div>
  );
}
