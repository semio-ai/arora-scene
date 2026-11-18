import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type FormEvent,
} from "react";
import { createRoot } from "react-dom/client";

import {
  Scene,
  type Backend,
  type Joint,
  type OverlayFrame,
  type SceneEvent,
  type SceneHandle,
} from "../src";

/** A robot's bytes and the key that names the model. */
interface Robot {
  glb: Uint8Array;
  key: string;
}

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
  const [robot, setRobot] = useState<Robot>();
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
        .then((bytes) => setRobot({ glb: new Uint8Array(bytes), key: url }))
        .catch((e: unknown) => setError(String(e)));
  }, []);

  if (!robot)
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
            if (file)
              setRobot({
                glb: new Uint8Array(await file.arrayBuffer()),
                key: `${file.name} ${file.size} ${file.lastModified}`,
              });
          }}
        />
        {error && <p style={{ color: "#f87171" }}>{error}</p>}
      </div>
    );
  return <Running robot={robot} />;
}

function Running({ robot: first }: { robot: Robot }) {
  // A robot under a different key here swaps it in the running scene.
  const [robot, setRobot] = useState(first);
  const [joints, setJoints] = useState<Joint[]>([]);
  const [pose, setPose] = useState<string>();
  const [scene, setScene] = useState<SceneHandle>();
  const [overlay, setOverlay] = useState<OverlayFrame>();
  const [log, setLog] = useState<string[]>([]);
  // Unmounting and mounting again shows the same running scene.
  const [shown, setShown] = useState(true);

  // This page plays the driver: a drag moves nothing until the driver writes
  // it back, which it does here unless told not to.
  const sceneRef = useRef<SceneHandle>(undefined);
  const writeBack = useRef(true);
  const note = (line: string) => setLog((l) => [line, ...l].slice(0, 8));

  const onEvent = useCallback((event: SceneEvent) => {
    if (event.type === "overlay") return setOverlay(event);
    if (event.type === "pick")
      return note(`pick ${event.instance} ${event.element ?? "(no element)"}`);
    if (event.type === "robot") {
      if (event.joints) setJoints(event.joints);
      return note(
        event.error
          ? `robot ${event.key} refused: ${event.error}`
          : `robot ${event.key} showing`,
      );
    }
    if (event.type === "controlPress")
      return note(
        `${event.button} press on ${event.key.slice(0, 8)} ${JSON.stringify(
          event.handle,
        )} at ${event.position.map((v) => Math.round(v)).join(",")}`,
      );
    const scene = sceneRef.current;
    if (event.phase !== "move")
      note(
        `${event.phase} ${event.key.slice(0, 8)} ${
          event.value?.toFixed(3) ??
          event.pose?.position.map((v) => v.toFixed(2)).join(" ")
        }`,
      );
    if (!scene || !writeBack.current) return;
    if (event.value !== undefined) scene.setValue(event.key, event.value);
    if (event.pose)
      scene.control({
        op: "setPose",
        key: event.key,
        pose: event.pose,
        scale: event.scale,
      });
  }, []);

  const ready = (handle: SceneHandle) => {
    sceneRef.current = handle;
    setScene(handle);
  };

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
      const feature = String(form.get("feature"));
      scene.setValue(String(form.get("target")), value, feature || undefined);
    } catch (err) {
      note(`error ${String(err)}`);
    }
  };

  const addControl = (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    const submitter = (e.nativeEvent as SubmitEvent).submitter as
      HTMLButtonElement | undefined;
    const key = String(new FormData(e.currentTarget).get("key"));
    if (!scene || !key) return;
    switch (submitter?.value) {
      case "joint":
        return scene.control({ op: "add", key, control: { kind: "joint" } });
      case "goal":
        return scene.control({
          op: "add",
          key,
          control: {
            kind: "transform",
            target: { kind: "goal", pose: { position: [0.3, 0, 1.2] } },
          },
        });
      case "remove":
        return scene.control({ op: "remove", key });
      case "highlight":
      case "unhighlight":
        return scene.control({
          op: "highlight",
          key,
          on: submitter?.value === "highlight",
        });
      case "recolour":
        // Every kind takes the colour it can: a joint one, a transform three.
        return scene.control({
          op: "configure",
          key,
          color: "#f59e0b",
          colors: ["#f59e0b", "#a855f7", "#14b8a6"],
        });
      case "local":
      case "world":
        return scene.control({
          op: "configure",
          key,
          local: submitter?.value === "local",
        });
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
          glb={robot.glb}
          robotKey={robot.key}
          backend={backendParam()}
          options={{ environment: "sky" }}
          onReady={ready}
          onError={(error) => note(`error ${String(error)}`)}
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
          <input name="feature" placeholder="feature, for an element id" />
          <input name="value" placeholder="one number, or three" required />
          <button type="submit" disabled={!scene}>
            Write value
          </button>
        </form>
        <h4>Robot</h4>
        <div style={{ display: "grid", gap: 8 }}>
          <input
            type="file"
            accept=".glb,model/gltf-binary"
            onChange={async (e) => {
              const file = e.target.files?.[0];
              if (file)
                setRobot({
                  glb: new Uint8Array(await file.arrayBuffer()),
                  key: `${file.name} ${file.size} ${file.lastModified}`,
                });
            }}
          />
          <button
            type="button"
            disabled={!scene}
            onClick={() =>
              setRobot((r) => ({ glb: r.glb, key: `${r.key} (again)` }))
            }
          >
            Load this robot again, under a new key
          </button>
          <form
            style={{ display: "flex", gap: 8 }}
            onSubmit={async (e) => {
              e.preventDefault();
              const url = String(new FormData(e.currentTarget).get("url"));
              const response = await fetch(url);
              if (!response.ok) return note(`${url}: ${response.status}`);
              setRobot({
                glb: new Uint8Array(await response.arrayBuffer()),
                key: url,
              });
            }}
          >
            <input name="url" placeholder="robot URL" required />
            <button type="submit" disabled={!scene}>
              Load
            </button>
          </form>
        </div>
        <h4>Controls</h4>
        <form onSubmit={addControl} style={{ display: "grid", gap: 8 }}>
          <input
            name="key"
            placeholder="joint value animatable, or any key"
            required
          />
          <button type="submit" value="joint" disabled={!scene}>
            Add a joint control
          </button>
          <button type="submit" value="goal" disabled={!scene}>
            Add a goal
          </button>
          <button type="submit" value="remove" disabled={!scene}>
            Remove
          </button>
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <button type="submit" value="highlight" disabled={!scene}>
              Highlight
            </button>
            <button type="submit" value="unhighlight" disabled={!scene}>
              Unhighlight
            </button>
            <button type="submit" value="recolour" disabled={!scene}>
              Recolour
            </button>
            <button type="submit" value="local" disabled={!scene}>
              Local axes
            </button>
            <button type="submit" value="world" disabled={!scene}>
              World axes
            </button>
          </div>
          <label>
            <input
              type="checkbox"
              defaultChecked
              onChange={(e) => (writeBack.current = e.target.checked)}
            />{" "}
            Write drags back, as a driver would
          </label>
        </form>
        <h4>Joints</h4>
        <ul style={{ paddingLeft: 16 }}>
          {joints.map((joint) => (
            <li key={joint.name}>
              {joint.name} ({joint.kind})
              {joint.animatable && <code> {joint.animatable.slice(0, 8)}</code>}
              {joint.mimic && " mimics"}
            </li>
          ))}
        </ul>
        <form
          style={{ display: "flex", gap: 8 }}
          onSubmit={(e) => {
            e.preventDefault();
            const element = String(
              new FormData(e.currentTarget).get("element"),
            );
            const found = scene?.elementPose(element);
            setPose(found ? JSON.stringify(found) : "no such element");
          }}
        >
          <input name="element" placeholder="element id" required />
          <button type="submit" disabled={!scene}>
            Where is it?
          </button>
        </form>
        {pose && <pre style={{ whiteSpace: "pre-wrap" }}>{pose}</pre>}
        <h4>Pointer</h4>
        <pre>{hovered ? JSON.stringify(hovered, null, 1) : "none"}</pre>
        <h4>Events</h4>
        <pre style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}>
          {log.join("\n") || "none yet"}
        </pre>
        <p>{overlay ? `${overlay.elements.length} elements on screen` : ""}</p>
      </aside>
    </div>
  );
}

createRoot(document.getElementById("root")!).render(<Example />);
