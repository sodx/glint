export declare const VERSION: string;

export type Color = "gray" | "green" | "orange" | "red" | "blue" | "purple";
export type Level = "trace" | "debug" | "info" | "warn" | "error" | "fatal";

/** Handle for events just sent; its methods change how they are shown in Glint. */
export declare class Entry {
  readonly ids: string[];
  label(label: string): this;
  color(color: Color): this;
  gray(): this;
  green(): this;
  orange(): this;
  red(): this;
  blue(): this;
  purple(): this;
  small(): this;
  large(): this;
  hide(): this;
  remove(): this;
}

export interface Glint {
  /** Sends each value as its own `core.value` event. */
  (...values: unknown[]): Entry;
  log(message: unknown, fields?: Record<string, unknown> | null, level?: Level): Entry;
  table(rows: Iterable<unknown>, columns?: Iterable<string>): Entry;
  exception(error: unknown): Entry;
  /** Runs `fn` (sync or async), sends its duration, returns its result. */
  measure<T>(name: string, fn: () => T): T;
  /** Sends any kind as-is, e.g. `glint.send("x-acme.order", {...})`. */
  send(kind: string, payload: unknown, meta?: Record<string, unknown>): Entry;
  /**
   * Waits until someone continues the program from the Glint UI or an agent; resolves with the
   * value they passed. Throws GlintStop if they chose "stop". No-op on CI unless GLINT_FORCE_PAUSE=1.
   */
  pause<T = unknown>(reason?: string, options?: { timeoutMs?: number }): Promise<T | undefined>;
  /** Resolves once queued events are sent, or after `timeoutMs`. */
  flush(timeoutMs?: number): Promise<void>;
  configure(options: { url?: string; enabled?: boolean; project?: string }): void;
}

export declare class GlintStop extends Error {}

export declare const glint: Glint;
export default glint;

export interface Frame {
  file: string;
  line: number;
  col: number;
  fn?: string;
  vendor?: boolean;
}
export declare function parseStack(stack: string | undefined): Frame[];
