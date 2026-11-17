import { useCallback, useEffect, useState, type FormEvent } from "react";
import { createRoot } from "react-dom/client";

import {
  Scene,
  type Backend,
  type OverlayFrame,
  type SceneEvent,
  type SceneHandle,
} from "../src";

/** `?backend=webgl2` or `?backend=webgpu` forces one; otherwise the page chooses. */
function backendParam(): Backend | undefined {
  const asked = new URLSearchParams(location.search).get("backend");
  return asked === "webgpu" || asked === "webgl2" ? asked : undefined;
}

/**
 * A page for driving the scene by hand: load a robot, write values to it, and
 * watch what it reports. Open it with `?glb=<url>` to load a robot from a
 * URL, or choose a file.
 */
function Example() {
  const [glb, setGlb] = useState<Uint8Array>();
  const [error, setError] = useState<string>();

  // A robot named in the URL loads without a click.
  useEffect(() => {
    const url = new URLSearchParams(location.search).get("glb");
    if (url)
      fetch(url)
        .then((r) =>
          r.ok
            ? r.arrayBuffer()
            : Promise.reject(new Error(`${url}: ${r.status}`)),
        )
        .then((bytes) => setGlb(new Uint8Array(bytes)))
        .catch((e: unknown) => setError(String(e)));
  }, []);

  if (!glb)
    return (
      <div style={{ padding: 24 }}>
        <p>
          Choose a robot GLB, or open this page with{" "}
          <code>?glb=&lt;url&gt;</code>.
        </p>
        <input
          type="file"
          accept=".glb,model/gltf-binary"
          onChange={async (e) => {
            const file = e.target.files?.[0];
            if (file) setGlb(new Uint8Array(await file.arrayBuffer()));
          }}
        />
        {error && <p style={{ color: "#f87171" }}>{error}</p>}
      </div>
    );
  return <Running glb={glb} />;
}

function Running({ glb }: { glb: Uint8Array }) {
  const [scene, setScene] = useState<SceneHandle>();
  const [overlay, setOverlay] = useState<OverlayFrame>();
  const [log, setLog] = useState<string[]>([]);
  // Unmounting and mounting again shows the same running scene.
  const [shown, setShown] = useState(true);

  const onEvent = useCallback((event: SceneEvent) => {
    if (event.type === "overlay") setOverlay(event);
    else
      setLog((l) =>
        [
          `pick ${event.instance} ${event.element ?? "(no element)"}`,
          ...l,
        ].slice(0, 8),
      );
  }, []);

  const write = (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    const form = new FormData(e.currentTarget);
    const numbers = String(form.get("value"))
      .split(/[\s,]+/)
      .filter(Boolean)
      .map(Number);
    const value =
      numbers.length === 3 ? (numbers as [number, number, number]) : numbers[0];
    if (!scene || value === undefined) return;
    try {
      scene.setValue(
        String(form.get("target")),
        String(form.get("feature")),
        value,
      );
    } catch (err) {
      setLog((l) => [`error ${String(err)}`, ...l].slice(0, 8));
    }
  };

  const hovered = overlay?.pointer;
  return (
    <div
      style={{
        display: "grid",
        gridTemplateColumns: "1fr 280px",
        height: "100%",
      }}
    >
      {shown ? (
        <Scene
          glb={glb}
          backend={backendParam()}
          options={{ environment: "sky" }}
          onReady={setScene}
          onEvent={onEvent}
          style={{ height: "100%" }}
        />
      ) : (
        <div style={{ padding: 24 }}>The scene is parked, still running.</div>
      )}
      <aside
        style={{ padding: 16, overflow: "auto", borderLeft: "1px solid #333" }}
      >
        <p>Backend: {scene?.backend ?? "starting"}</p>
        <button type="button" onClick={() => setShown((s) => !s)}>
          {shown ? "Unmount the scene" : "Mount the scene"}
        </button>
        <form onSubmit={write} style={{ display: "grid", gap: 8 }}>
          <input
            name="target"
            placeholder="animatable or element id"
            required
          />
          <input
            name="feature"
            placeholder="feature, e.g. rotation"
            defaultValue="rotation"
            required
          />
          <input name="value" placeholder="one number, or three" required />
          <button type="submit" disabled={!scene}>
            Write value
          </button>
        </form>
        <h4>Pointer</h4>
        <pre>{hovered ? JSON.stringify(hovered, null, 1) : "none"}</pre>
        <h4>Events</h4>
        <pre>{log.join("\n") || "none yet"}</pre>
        <p>{overlay ? `${overlay.elements.length} elements on screen` : ""}</p>
      </aside>
    </div>
  );
}

createRoot(document.getElementById("root")!).render(<Example />);
