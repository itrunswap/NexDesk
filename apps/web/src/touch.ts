export type Point = { x: number; y: number };
export type TouchMode = "trackpad" | "direct";
export type Viewport = Point & { zoom: number };

type TouchActions = {
  move: (point: Point) => void;
  button: (point: Point, button: number, down: boolean) => boolean;
  scroll: (delta: Point) => void;
  view: (viewport: Viewport) => void;
  release: () => void;
};

const center = (a: Point, b: Point) => ({
  x: (a.x + b.x) / 2,
  y: (a.y + b.y) / 2,
});
const distance = (a: Point, b: Point) => Math.hypot(a.x - b.x, a.y - b.y);
const subtract = (a: Point, b: Point) => ({ x: a.x - b.x, y: a.y - b.y });
const bound = (value: number, max: number) =>
  Math.min(max, Math.max(-max, value));
const normalize = (value: number) => Math.max(0, Math.min(65535, value));

/** Convert between capture coordinates and the video rectangle after letterboxing. */
export function videoBounds(
  rect: { left: number; top: number; width: number; height: number },
  video: { width: number; height: number },
) {
  if (!video.width || !video.height || !rect.width || !rect.height) return;
  const scale = Math.min(rect.width / video.width, rect.height / video.height);
  const width = video.width * scale;
  const height = video.height * scale;
  return {
    x: rect.left + (rect.width - width) / 2,
    y: rect.top + (rect.height - height) / 2,
    width,
    height,
  };
}

export function constrainView(view: Viewport, size: Point): Viewport {
  const zoom = Math.max(1, Math.min(4, view.zoom));
  return {
    x: bound(view.x, (size.x * (zoom - 1)) / 2),
    y: bound(view.y, (size.y * (zoom - 1)) / 2),
    zoom,
  };
}

/** Pointer gestures share the existing authenticated move, button and wheel commands. */
export class TouchInput {
  private points = new Map<number, Point>();
  private tap?: { start: Point; time: number };
  private pinch?: {
    center: Point;
    span: number;
    initial: number;
    scaling: boolean;
  };
  private actions: TouchActions;
  private cursor: Point = { x: 32768, y: 32768 };
  private view: Viewport = { zoom: 1, x: 0, y: 0 };
  private drag = false;

  constructor(actions: TouchActions) {
    this.actions = actions;
  }

  down(id: number, at: Point, time: number) {
    if (this.points.size >= 2) return false;
    this.points.set(id, at);
    const [first, second] = this.points.values();
    if (first && second) {
      this.tap = undefined;
      this.pinch = {
        center: center(first, second),
        span: Math.max(12, distance(first, second)),
        initial: this.view.zoom,
        scaling: false,
      };
    } else {
      this.tap = { start: at, time };
    }
    return true;
  }

  move(
    id: number,
    at: Point,
    size: Point,
    mode: TouchMode,
    direct: (point: Point) => Point | undefined,
    canInput: boolean,
  ) {
    const last = this.points.get(id);
    if (!last) return;
    this.points.set(id, at);
    const [first, second] = this.points.values();
    if (first && second && this.pinch) {
      this.movePair(first, second, size, canInput);
      return;
    }
    if (this.pinch || (this.tap && distance(at, this.tap.start) <= 5)) return;
    this.tap = undefined;
    if (!canInput) return;
    const delta = subtract(at, last);
    const target =
      mode === "direct"
        ? direct(at)
        : {
            x: normalize(
              this.cursor.x + (1.15 * 65535 * delta.x) / Math.max(180, size.x),
            ),
            y: normalize(
              this.cursor.y + (1.15 * 65535 * delta.y) / Math.max(180, size.y),
            ),
          };
    if (target) this.moveCursor(target);
  }

  private movePair(
    first: Point,
    second: Point,
    size: Point,
    canInput: boolean,
  ) {
    const gesture = this.pinch;
    if (!gesture) return;
    const next = center(first, second);
    const delta = subtract(next, gesture.center);
    const span = distance(first, second);
    gesture.center = next;
    if (Math.abs(span - gesture.span) > 8) gesture.scaling = true;
    if (gesture.scaling || this.view.zoom > 1) {
      this.view = constrainView(
        {
          zoom: gesture.scaling
            ? (gesture.initial * span) / gesture.span
            : this.view.zoom,
          x: this.view.x + delta.x,
          y: this.view.y + delta.y,
        },
        size,
      );
      this.actions.view(this.view);
    } else if (canInput) {
      this.actions.scroll({ x: -delta.x * 3, y: -delta.y * 3 });
    }
  }

  up(
    id: number,
    at: Point,
    time: number,
    mode: TouchMode,
    direct: (point: Point) => Point | undefined,
    canInput: boolean,
  ) {
    if (!this.points.delete(id)) return;
    if (this.pinch) {
      if (this.points.size === 0) this.pinch = undefined;
      return;
    }
    const tap = this.tap;
    this.tap = undefined;
    if (!tap || distance(tap.start, at) > 5 || this.drag || !canInput) return;
    const target = mode === "direct" ? direct(at) : this.cursor;
    if (!target) return;
    this.moveCursor(target);
    const button = time - tap.time >= 500 ? 2 : 1;
    if (this.actions.button(target, button, true)) {
      this.actions.button(target, button, false);
    }
  }

  moveCursor(point: Point) {
    this.cursor = point;
    this.actions.move(point);
  }

  toggleDrag() {
    if (this.drag) {
      this.drag = false;
      this.actions.release();
    } else {
      this.drag = this.actions.button(this.cursor, 1, true);
    }
    return this.drag;
  }

  resetZoom() {
    this.view = { zoom: 1, x: 0, y: 0 };
    this.actions.view(this.view);
  }

  resize(size: Point) {
    const next = constrainView(this.view, size);
    if (next.x === this.view.x && next.y === this.view.y) return;
    this.view = next;
    this.actions.view(next);
  }

  ownsPointer(id: number) {
    return this.points.has(id);
  }

  cancel() {
    this.points.clear();
    this.tap = undefined;
    this.pinch = undefined;
    this.drag = false;
    this.actions.release();
  }
}
