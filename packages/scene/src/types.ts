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
  /** Show the transform gizmo. */
  gizmo?: boolean;
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

/** Something the scene reports, by its `type`. */
export type SceneEvent =
  /** Sent each frame the overlay changed. */
  | ({ type: "overlay" } & OverlayFrame)
  /**
   * A press on the robot: the element under it, or `null` for a press that
   * hit no element.
   */
  | { type: "pick"; instance: string; element: string | null };

/**
 * A value for one feature: one number (a joint's angle or travel) or three
 * (a translation, an euler rotation, a linear RGB colour).
 */
export type FeatureValue = number | [number, number, number];
