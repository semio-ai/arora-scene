/** Which GPU interface a scene draws with. */
export type Backend = "webgpu" | "webgl2";

/** How a scene starts. Every field is optional. */
export interface SceneOptions {
  /** Cast shadows. On unless turned off. */
  shadows?: boolean;
  theme?: "dark" | "light";
  /**
   * `"sky"` shows a sky, with an aurora under the dark theme; `"plain"` (the
   * default) is the ground colour all the way up.
   */
  environment?: "plain" | "sky";
  /** Element ids to select from the start. */
  select?: string[];
  /**
   * The part of the canvas left visible beside the host's panels,
   * `[x, y, width, height]` in CSS pixels. The camera frames the robot in it.
   * The whole canvas when absent.
   */
  activeArea?: [number, number, number, number];
  /** Physical pixels per CSS pixel, instead of `devicePixelRatio`. */
  scaleFactor?: number;
  /** The camera's starting height above the model, in radians. */
  pitch?: number;
  /** The camera's starting yaw about the model, in radians. */
  angle?: number;
}

/**
 * A box on screen, in CSS pixels of the canvas, with its centre at `x`, `y`.
 */
export interface ScreenBox {
  x: number;
  y: number;
  min_x: number;
  min_y: number;
  max_x: number;
  max_y: number;
}

/** One element's box on screen. */
export interface ElementAnchor extends ScreenBox {
  instance: string;
  /** The element's id, as the robot's `RobotData` declares it. */
  element: string;
  /** The name of the element's glTF node. */
  node: string;
}

export type GizmoKind = "Transform" | "Joint";

/** One gizmo's box on screen, around everything it draws. */
export interface GizmoAnchor {
  /** The key the host gave the gizmo. */
  key: string;
  kind: GizmoKind;
  screen: ScreenBox;
}

/** A transform gizmo's handle, by axis (0, 1, 2 for X, Y, Z), or a joint's. */
export type GizmoHandle =
  | { Arrow: number }
  | { Plane: number }
  | { Ring: number }
  | { Ball: number }
  | "Joint";

/** One amount a gizmo reads, along or about one axis. */
export interface Reading {
  /** 0, 1 or 2 for X, Y or Z; `null` for a joint, whose axis is its own. */
  axis: number | null;
  amount: number;
  unit: "Metres" | "Degrees" | "Factor";
}

/** The gizmo the pointer has. */
export interface GizmoInHand {
  key: string;
  kind: GizmoKind;
  handle: GizmoHandle;
  /** Held in a drag, rather than only under the pointer. */
  held: boolean;
  readings: Reading[];
}

/** What a host needs to draw labels and tooltips over the scene. */
export interface OverlayFrame {
  elements: ElementAnchor[];
  gizmos: GizmoAnchor[];
  pointer: GizmoInHand | null;
}

/**
 * A pose as a driver sees it: Z up, in metres, with the rotation as a
 * quaternion `[x, y, z, w]`.
 */
export interface Pose {
  position: [number, number, number];
  /** No turn when absent. */
  rotation?: [number, number, number, number];
}

/** What a transform control moves. */
export type ControlTarget =
  | { kind: "robot" }
  /** One element, by the id the robot's `RobotData` gives it. */
  | { kind: "element"; id: string }
  /** A goal at `pose`, with nothing drawn but the control: an IK target. */
  | { kind: "goal"; pose: Pose };

/** A control a driver adds. */
export type Control =
  /** A ring or bar on the joint whose value animatable is the control's key. */
  | {
      kind: "joint";
      /** A CSS hex colour (`"#22c55e"`); the joint's own when absent. */
      color?: string;
    }
  | {
      kind: "transform";
      target: ControlTarget;
      /** Every mode when absent. */
      modes?: ("translate" | "rotate" | "scale")[];
      /** Handles along the target's own axes rather than the world's. */
      local?: boolean;
      /** The X, Y and Z handles' CSS hex colours; red, green, blue when absent. */
      colors?: [string, string, string];
    };

/** An instruction about one control, by the driver's key for it. */
export type ControlCommand =
  | { op: "add"; key: string; control: Control }
  | { op: "remove"; key: string }
  | { op: "show"; key: string; visible: boolean }
  /** Moves a transform control's target to `pose`, in the world. */
  | {
      op: "setPose";
      key: string;
      pose: Pose;
      /** The target's scale along its own axes; kept as it is when absent. */
      scale?: [number, number, number];
    }
  /**
   * Changes a control in place: a joint control's `color`, a transform
   * control's `colors` and `local`. What is absent stays as it is.
   */
  | {
      op: "configure";
      key: string;
      color?: string;
      colors?: [string, string, string];
      local?: boolean;
    }
  /**
   * Draws a control as the one under the pointer is drawn, or stops: for a
   * host pointing at it from elsewhere, such as its row in a list. `handle`
   * names one handle of a transform control, as the overlay names it; the
   * whole control when absent.
   */
  | { op: "highlight"; key: string; handle?: GizmoHandle; on: boolean };

/**
 * A drag on a control: what it asks its target to become. **Nothing has
 * moved**: the driver writes the joint's value, or sets the pose, if it wants
 * the change.
 */
export interface ControlEvent {
  key: string;
  phase: "start" | "move" | "end";
  /** A joint control's value. */
  value?: number;
  /** A transform control's target's pose, in the world. */
  pose?: Required<Pose>;
  /** A transform control's target's scale, along its own axes. */
  scale?: [number, number, number];
}

/** Something the scene reports, by its `type`. */
export type SceneEvent =
  /** Sent each frame the overlay changed. */
  | ({ type: "overlay" } & OverlayFrame)
  /**
   * A press on the robot: the element under it, or `null` for a press that
   * hit no element.
   */
  | { type: "pick"; instance: string; element: string | null }
  /** A drag on a control the driver added. */
  | ({ type: "control" } & ControlEvent)
  /**
   * A press with a button other than the primary on a control's handle,
   * which is the host's to act on: it moves nothing and turns no view.
   * `position` is in the canvas's CSS pixels. A right-click over a control
   * also reaches the page as an ordinary `contextmenu` event, not prevented,
   * so a context menu wrapped around `<Scene>` opens there.
   */
  | {
      type: "controlPress";
      key: string;
      handle: GizmoHandle;
      button: "secondary" | "middle";
      position: [number, number];
    }
  /**
   * How a robot the scene was given ended up, the one it started with
   * included, by the key the host gave it. Once it is showing, `error` is
   * absent, `joints` lists its joints and `elements` its elements, each with
   * the name its author gave it (`""` where unnamed). Otherwise `error` says
   * why it could not be loaded or what replaced it, the old robot still
   * showing. `loadRobot`'s promise settles from this.
   */
  | {
      type: "robot";
      key: string;
      error?: string;
      joints?: Joint[];
      elements?: { id: string; name: string }[];
    };

/** The robot showing, as its `robot` event described it. */
export interface ShownRobot {
  key: string;
  joints: Joint[];
  /** Each element's id with the name its author gave it, `""` where unnamed. */
  elements: { id: string; name: string }[];
}

/** One of a robot's joints, as its `RobotData` declares it. */
export interface Joint {
  name: string;
  kind: "revolute" | "continuous" | "prismatic" | "fixed";
  /** The element the joint moves, by its `RobotData` id. */
  element: string;
  /**
   * The animatable to write with `setValue` to move it, and the key of its
   * joint control. Absent for a joint that is not animated; joints that mimic
   * one another share their leader's.
   */
  animatable?: string;
  /**
   * Present on a joint that follows another: it stands at
   * `value * multiplier + offset` of its animatable's value.
   */
  mimic?: { multiplier: number; offset: number };
  /** The limits, absent where the joint has none. */
  min?: number;
  max?: number;
  default: number;
}

/**
 * A value for one feature: one number (a joint's angle or travel) or three
 * (a translation, an euler rotation, a linear RGB colour).
 */
export type FeatureValue = number | [number, number, number];
