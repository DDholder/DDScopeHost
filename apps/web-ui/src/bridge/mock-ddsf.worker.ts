import { MockSimulationCore } from "./mock-ddsf-core.ts";

type WorkerInboundMessage =
  | { type: "start"; rateHz: number }
  | { type: "stop" }
  | { type: "setRate"; rateHz: number }
  | { type: "rpc"; requestId: number; method: string; args: unknown[] };

const core = new MockSimulationCore();

let timerId: ReturnType<typeof setInterval> | null = null;
let targetRateHz = 120;
let lastTickMs = performance.now();

const postWorkerMessage = (
  message: unknown,
  transfer?: ArrayBuffer[],
): void => {
  (self as unknown as { postMessage: (msg: unknown, transfer?: Transferable[]) => void }).postMessage(
    message,
    transfer,
  );
};

function emitStep(): void {
  const now = performance.now();
  const dtMs = Math.max(1, Math.min(50, now - lastTickMs));
  lastTickMs = now;

  const result = core.step(dtMs, Date.now());
  const downlinkStats = core.getDownlinkStats();
  postWorkerMessage(
    {
      type: "frame",
      frameBuffer: result.frameBuffer,
      telemetry: result.telemetry,
      downlinkStats,
    },
    [result.frameBuffer],
  );
}

function restartTimer(rateHz: number): void {
  targetRateHz = Math.max(15, Math.min(240, rateHz));
  if (timerId !== null) {
    clearInterval(timerId);
    timerId = null;
  }
  const intervalMs = Math.max(4, Math.round(1000 / targetRateHz));
  lastTickMs = performance.now();
  timerId = setInterval(emitStep, intervalMs);
}

function stopTimer(): void {
  if (timerId !== null) {
    clearInterval(timerId);
    timerId = null;
  }
}

self.onmessage = (ev: MessageEvent<WorkerInboundMessage>) => {
  const msg = ev.data;
  if (msg.type === "start") {
    restartTimer(msg.rateHz);
    emitStep();
    return;
  }
  if (msg.type === "stop") {
    stopTimer();
    return;
  }
  if (msg.type === "setRate") {
    if (timerId !== null) {
      restartTimer(msg.rateHz);
    } else {
      targetRateHz = msg.rateHz;
    }
    return;
  }
  if (msg.type === "rpc") {
    try {
      const target = core as unknown as Record<string, (...args: unknown[]) => unknown>;
      const fn = target[msg.method];
      if (typeof fn !== "function") {
        throw new Error(`Unknown MockSimulationCore method: ${msg.method}`);
      }
      const result = fn.apply(core, msg.args);
      postWorkerMessage({
        type: "rpcResult",
        requestId: msg.requestId,
        ok: true,
        result,
      });
    } catch (err: unknown) {
      postWorkerMessage({
        type: "rpcResult",
        requestId: msg.requestId,
        ok: false,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  }
};
