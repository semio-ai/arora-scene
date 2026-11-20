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
  type SceneRobot,
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
 * A page for driving the scene by hand: load robots, write values to them,
 * and watch what they report. Open it with `?glb=<url>` to load a robot from
 * a URL, or choose a file.
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

/** A joint control the page added: which joint of which robot it drives. */
interface JointControl {
  instance: string;
  joint: string;
}

function Running({ robot: first }: { robot: Robot }) {
  // The robots shown, by the page's own id for each instance. A robot under a
  // different key in an instance swaps it; an id gone unloads it.
  const [robots, setRobots] = useState<Record<string, SceneRobot>>({
    "robot-1": first,
  });
  // The instance the forms below act on.
  const [current, setCurrent] = useState("robot-1");
  const [joints, setJoints] = useState<Record<string, Joint[]>>({});
  const [pose, setPose] = useState<string>();
  const [scene, setScene] = useState<SceneHandle>();
  const [overlay, setOverlay] = useState<OverlayFrame>();
  const [log, setLog] = useState<string[]>([]);
  // Unmounting and mounting again shows the same running scene.
  const [shown, setShown] = useState(true);

  // This page plays the driver: a drag moves nothing until the driver writes
  // it back, which it does here unless told not to. A joint control's drag
  // names its key, so the driver keeps which joint each key drives.
  const sceneRef = useRef<SceneHandle>(undefined);
  const writeBack = useRef(true);
  const jointControls = useRef(new Map<string, JointControl>());
  const note = (line: string) => setLog((l) => [line, ...l].slice(0, 8));

  const onEvent = useCallback((event: SceneEvent) => {
    if (event.type === "overlay") return setOverlay(event);
    if (event.type === "pick")
      return note(`pick ${event.instance} ${event.element ?? "(no element)"}`);
    if (event.type === "loaded") {
      setJoints((j) => ({ ...j, [event.instance]: event.joints }));
      return note(`${event.instance} shows ${event.key ?? "a robot"}`);
    }
    if (event.type === "ready") return note(`${event.instance} drawn`);
    if (event.type === "loadFailed")
      return note(`${event.instance} refused ${event.key}: ${event.error}`);
    if (event.type === "controlPress")
      return note(
        `${event.button} press on ${event.key} ${JSON.stringify(
          event.handle,
        )} at ${event.position.map((v) => Math.round(v)).join(",")}`,
      );
    const scene = sceneRef.current;
    if (event.phase !== "move")
      note(
        `${event.phase} ${event.key} ${
          event.value?.toFixed(3) ??
          event.pose?.position.map((v) => v.toFixed(2)).join(" ")
        }`,
      );
    if (!scene || !writeBack.current) return;
    const joint = jointControls.current.get(event.key);
    if (event.value !== undefined && joint)
      scene.setValue(joint.instance, joint.joint, event.value);
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

  // Another robot like the first, a metre along from the last one, under an
  // id not in use: counting the robots would name one still there once an
  // earlier one is removed.
  const added = useRef(0);
  const addRobot = () => {
    let id: string;
    do id = `robot-${++added.current + 1}`;
    while (Object.hasOwn(robots, id));
    // Spaced by the same count that names it, so a robot added after one was
    // removed does not stand where another already does.
    const along = added.current;
    setRobots((r) => ({
      ...r,
      [id]: { ...first, pose: { position: [along, 0, 0] } },
    }));
    setCurrent(id);
  };

  // Changes the robot in the current instance, if there is one.
  const change = (to: (robot: SceneRobot) => SceneRobot) =>
    setRobots((r) => {
      const robot = r[current];
      return robot ? { ...r, [current]: to(robot) } : r;
    });

  const removeRobot = () => {
    setRobots(({ [current]: _, ...rest }) => rest);
    setCurrent(Object.keys(robots).find((id) => id !== current) ?? "");
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
      scene.setValue(
        current,
        String(form.get("target")),
        value,
        feature || undefined,
      );
    } catch (err) {
      note(`error ${String(err)}`);
    }
  };

  const addControl = (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    const submitter = (e.nativeEvent as SubmitEvent).submitter as
      HTMLButtonElement | undefined;
    const form = new FormData(e.currentTarget);
    const key = String(form.get("key"));
    if (!scene || !key) return;
    switch (submitter?.value) {
      case "joint": {
        const joint = String(form.get("joint"));
        jointControls.current.set(key, { instance: current, joint });
        return scene.control({
          op: "add",
          key,
          control: { kind: "joint", instance: current, joint },
        });
      }
      case "robot":
        return scene.control({
          op: "add",
          key,
          control: {
            kind: "transform",
            target: { kind: "robot", instance: current },
          },
        });
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
        jointControls.current.delete(key);
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
      case "still":
      case "live":
        // What a host does around playback: dim and still a control, then
        // bring it back.
        return scene.control({
          op: "configure",
          key,
          opacity: submitter?.value === "still" ? 0.25 : 1,
          interactive: submitter?.value === "live",
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
        gridTemplateColumns: "1fr 300px",
        height: "100%",
      }}
    >
      {shown ? (
        <Scene
          robots={robots}
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
        <h4>Robots</h4>
        <div style={{ display: "grid", gap: 8 }}>
          <select value={current} onChange={(e) => setCurrent(e.target.value)}>
            {Object.keys(robots).map((id) => (
              <option key={id} value={id}>
                {id}
              </option>
            ))}
          </select>
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <button type="button" disabled={!scene} onClick={addRobot}>
              Add another
            </button>
            <button
              type="button"
              disabled={!scene || !current}
              onClick={removeRobot}
            >
              Remove this one
            </button>
            <button
              type="button"
              disabled={!scene || !current}
              onClick={() =>
                change((robot) => ({ ...robot, key: `${robot.key}+` }))
              }
            >
              Swap it, under a new key
            </button>
          </div>
          <input
            type="file"
            accept=".glb,model/gltf-binary"
            onChange={async (e) => {
              const file = e.target.files?.[0];
              if (!file) return;
              const glb = new Uint8Array(await file.arrayBuffer());
              const key = `${file.name} ${file.size} ${file.lastModified}`;
              change((robot) => ({ ...robot, glb, key }));
            }}
          />
          <button
            type="button"
            disabled={!scene || !current}
            onClick={() =>
              scene?.setSelection(
                current,
                (scene.robots.get(current)?.elements ?? [])
                  .slice(0, 1)
                  .map((e) => e.id),
              )
            }
          >
            Select its first element
          </button>
        </div>
        <h4>Values</h4>
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
        <h4>Controls</h4>
        <form onSubmit={addControl} style={{ display: "grid", gap: 8 }}>
          <input name="key" placeholder="the control's key" required />
          <input name="joint" placeholder="joint value animatable" />
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <button type="submit" value="joint" disabled={!scene}>
              Joint control
            </button>
            <button type="submit" value="robot" disabled={!scene}>
              Move the robot
            </button>
            <button type="submit" value="goal" disabled={!scene}>
              Goal
            </button>
            <button type="submit" value="remove" disabled={!scene}>
              Remove
            </button>
          </div>
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
            <button type="submit" value="still" disabled={!scene}>
              Dim and still
            </button>
            <button type="submit" value="live" disabled={!scene}>
              Bring back
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
        <h4>Joints of {current}</h4>
        <ul style={{ paddingLeft: 16 }}>
          {(joints[current] ?? []).map((joint) => (
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
            const found = scene?.elementPose(current, element);
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
