import { Channel } from "@tauri-apps/api/core";
import {
  commands,
  type DataBridgeAttachDto,
  type DataBridgeModeDto,
  type DataBridgeStatsDto_Serialize,
  type DdsfSlotReadyDto,
} from "./bindings.ts";
import {
  DDSF_HEADER_BYTES,
  DDSF_MAGIC,
  DDSF_SHM_SLOT_COUNT,
  DdsfHeaderOffsets,
} from "./ddsf-spec.ts";
import { globalTopicBus, type HighFreqTopicBus } from "./topic-bus.ts";

/** Protocol identifier stamped into WebView2 `PostSharedBufferToScript` JSON metadata. */
export const DDSF_SHM_PROTOCOL_TAG = "ddscope-ddsf-shm-v1";

export interface WebView2SharedBufferMetadata {
  protocol?: string;
  slot?: number;
  capacityBytes?: number;
}

export interface WebView2SharedBufferReceivedEvent {
  getBuffer(): ArrayBuffer;
  readonly additionalData?: unknown;
}

export interface WebView2Host {
  addEventListener(
    type: "sharedbufferreceived",
    listener: (event: WebView2SharedBufferReceivedEvent) => void,
  ): void;
  removeEventListener(
    type: "sharedbufferreceived",
    listener: (event: WebView2SharedBufferReceivedEvent) => void,
  ): void;
  releaseBuffer?(buffer: ArrayBuffer): void;
}

export interface ChannelLike<T> {
  onmessage: ((response: T) => void) | null;
}

export interface ZeroCopyBridgeHostAdapter {
  getWebView2(): WebView2Host | null;
  createSlotChannel(): ChannelLike<DdsfSlotReadyDto>;
  createRawFrameChannel(): ChannelLike<unknown>;
  attachDataBridge(
    preferredMode: DataBridgeModeDto | null,
    slotNotifyChannel: ChannelLike<DdsfSlotReadyDto>,
    rawFrameChannel: ChannelLike<unknown>,
  ): Promise<DataBridgeAttachDto>;
  detachDataBridge(): Promise<void>;
  getDataBridgeStats(): Promise<DataBridgeStatsDto_Serialize>;
  setDataBridgeRate(rateHz: number): Promise<number>;
}

export interface ZeroCopyReceiverStats {
  attached: boolean;
  activeMode: DataBridgeModeDto | null;
  shmFramesIngested: number;
  rawFramesIngested: number;
  staleSlotDrops: number;
  tornSlotDetections: number;
  mappedSlots: number;
}

function defaultTauriWebView2(): WebView2Host | null {
  if (typeof globalThis === "undefined") {
    return null;
  }
  const win = globalThis as { chrome?: { webview?: WebView2Host } };
  const webview = win.chrome?.webview;
  if (
    webview &&
    typeof webview.addEventListener === "function" &&
    typeof webview.removeEventListener === "function"
  ) {
    return webview;
  }
  return null;
}

export function createTauriZeroCopyHostAdapter(): ZeroCopyBridgeHostAdapter {
  return {
    getWebView2: defaultTauriWebView2,
    createSlotChannel: () => new Channel<DdsfSlotReadyDto>(),
    createRawFrameChannel: () =>
      new Channel<number[]>() as unknown as ChannelLike<unknown>,
    attachDataBridge: (preferredMode, slotNotifyChannel, rawFrameChannel) =>
      commands.attachDataBridge(
        preferredMode,
        slotNotifyChannel as Channel<DdsfSlotReadyDto>,
        rawFrameChannel as Channel<number[]>,
      ),
    detachDataBridge: async () => {
      await commands.detachDataBridge();
    },
    getDataBridgeStats: () => commands.getDataBridgeStats(),
    setDataBridgeRate: (rateHz) => commands.setDataBridgeRate(rateHz),
  };
}

function parseSharedBufferMetadata(
  raw: unknown,
): WebView2SharedBufferMetadata | null {
  if (typeof raw === "string") {
    try {
      const parsed: unknown = JSON.parse(raw);
      return typeof parsed === "object" && parsed !== null
        ? (parsed as WebView2SharedBufferMetadata)
        : null;
    } catch {
      return null;
    }
  }
  if (typeof raw === "object" && raw !== null) {
    return raw as WebView2SharedBufferMetadata;
  }
  return null;
}

function createEmptySlotArray<T>(): (T | null)[] {
  return Array.from({ length: DDSF_SHM_SLOT_COUNT }, () => null);
}

/**
 * Frontend `TD2` zero-copy `DDSF v1` receiver.
 *
 * Registers WebView2 `sharedbufferreceived` listeners *before* calling `attach_data_bridge`,
 * queues early `DdsfSlotReadyDto` notifications per slot, validates seqlock `magic`/`frame_seq`
 * fences before and after `topicBus.ingestFrame` so producer wrap-around never causes false
 * parse errors, and falls back seamlessly to `InvokeResponseBody::Raw` binary Tauri channels.
 */
export class ZeroCopyDataBridgeReceiver {
  readonly topicBus: HighFreqTopicBus;
  private readonly host: ZeroCopyBridgeHostAdapter;

  private attached = false;
  private receiving = false;
  private attachPromise: Promise<DataBridgeAttachDto> | null = null;
  private activeMode: DataBridgeModeDto | null = null;

  private webview: WebView2Host | null = null;
  private shmListener: ((ev: WebView2SharedBufferReceivedEvent) => void) | null =
    null;
  private slotChannel: ChannelLike<DdsfSlotReadyDto> | null = null;
  private rawChannel: ChannelLike<unknown> | null = null;

  private slots: (ArrayBuffer | null)[] = createEmptySlotArray();
  private slotViews: (DataView | null)[] = createEmptySlotArray();
  private pendingSlotReady: (DdsfSlotReadyDto | null)[] = createEmptySlotArray();

  private shmFramesIngested = 0;
  private rawFramesIngested = 0;
  private staleSlotDrops = 0;
  private tornSlotDetections = 0;

  constructor(
    topicBus: HighFreqTopicBus = globalTopicBus,
    host: ZeroCopyBridgeHostAdapter = createTauriZeroCopyHostAdapter(),
  ) {
    this.topicBus = topicBus;
    this.host = host;
  }

  /**
   * Attaches the receiver to the host `ZeroCopyDataBridge`, preferring Windows WebView2
   * `SharedBuffer` when `preferredMode` is `null` or `"web_view2_shared_buffer"`.
   */
  async attach(
    preferredMode: DataBridgeModeDto | null = null,
  ): Promise<DataBridgeAttachDto> {
    if (this.attachPromise) {
      await this.attachPromise.catch(() => undefined);
    }
    if (this.attached || this.receiving) {
      await this.detach();
    }

    const runAttach = async (): Promise<DataBridgeAttachDto> => {
      this.resetSlotState();
      this.receiving = true;

      const slotChannel = this.host.createSlotChannel();
      const rawChannel = this.host.createRawFrameChannel();
      this.slotChannel = slotChannel;
      this.rawChannel = rawChannel;

      slotChannel.onmessage = (msg: DdsfSlotReadyDto) => {
        this.processSlotReady(msg);
      };
      rawChannel.onmessage = (payload: unknown) => {
        this.processRawFrame(payload);
      };

      const wantShm = preferredMode !== "raw_ipc_channel";
      const webview = wantShm ? this.host.getWebView2() : null;
      this.webview = webview;
      if (webview) {
        const listener = (ev: WebView2SharedBufferReceivedEvent) => {
          this.onSharedBufferReceived(ev);
        };
        this.shmListener = listener;
        // Must be registered BEFORE invoking `attach_data_bridge` so initial
        // `PostSharedBufferToScript` events are never missed.
        webview.addEventListener("sharedbufferreceived", listener);
      }

      try {
        const result = await this.host.attachDataBridge(
          preferredMode,
          slotChannel,
          rawChannel,
        );
        this.attached = true;
        this.activeMode = result.active_mode;

        if (result.active_mode === "raw_ipc_channel" && this.webview && this.shmListener) {
          this.webview.removeEventListener("sharedbufferreceived", this.shmListener);
          this.shmListener = null;
          this.releaseHeldBuffers();
        }
        return result;
      } catch (err) {
        this.cleanupClientResources();
        throw err;
      }
    };

    const promise = runAttach();
    this.attachPromise = promise;
    try {
      return await promise;
    } finally {
      if (this.attachPromise === promise) {
        this.attachPromise = null;
      }
    }
  }

  /**
   * Detaches listeners, releases mapped `SharedBuffer` handles, and notifies the host.
   */
  async detach(): Promise<void> {
    if (this.attachPromise) {
      await this.attachPromise.catch(() => undefined);
    }
    const wasActive = this.attached || this.receiving || this.shmListener !== null;
    this.cleanupClientResources();
    if (!wasActive) {
      return;
    }
    try {
      await this.host.detachDataBridge();
    } catch {
      // Ignore detach errors if host window or stub bridge is already inactive.
    }
  }

  /**
   * Updates the host data bridge emission rate (`15..=240 Hz`).
   */
  setRate(rateHz: number): Promise<number> {
    return this.host.setDataBridgeRate(rateHz);
  }

  /**
   * Queries host-side `ZeroCopyDataBridge` delivery and fallback counters.
   */
  getHostStats(): Promise<DataBridgeStatsDto_Serialize> {
    return this.host.getDataBridgeStats();
  }

  /**
   * Returns client-side zero-copy receiver metrics.
   */
  getReceiverStats(): ZeroCopyReceiverStats {
    let mappedSlots = 0;
    for (let i = 0; i < this.slots.length; i += 1) {
      if (this.slots[i] !== null) {
        mappedSlots += 1;
      }
    }
    return {
      attached: this.attached,
      activeMode: this.activeMode,
      shmFramesIngested: this.shmFramesIngested,
      rawFramesIngested: this.rawFramesIngested,
      staleSlotDrops: this.staleSlotDrops,
      tornSlotDetections: this.tornSlotDetections,
      mappedSlots,
    };
  }

  private onSharedBufferReceived(ev: WebView2SharedBufferReceivedEvent): void {
    const meta = parseSharedBufferMetadata(ev.additionalData);
    if (!meta || meta.protocol !== DDSF_SHM_PROTOCOL_TAG) {
      return;
    }
    const slotIdx = meta.slot;
    if (
      typeof slotIdx !== "number" ||
      !Number.isInteger(slotIdx) ||
      slotIdx < 0 ||
      slotIdx >= DDSF_SHM_SLOT_COUNT
    ) {
      return;
    }

    const buf = ev.getBuffer();
    const prev = this.slots[slotIdx];
    if (prev && prev !== buf) {
      this.webview?.releaseBuffer?.(prev);
    }

    this.slots[slotIdx] = buf;
    this.slotViews[slotIdx] = new DataView(buf);

    const pending = this.pendingSlotReady[slotIdx];
    if (pending !== null) {
      this.pendingSlotReady[slotIdx] = null;
      this.processSlotReady(pending);
    }
  }

  private processSlotReady(msg: DdsfSlotReadyDto): void {
    if (!this.receiving) {
      return;
    }
    const slotIdx = msg.slot;
    if (
      !Number.isInteger(slotIdx) ||
      slotIdx < 0 ||
      slotIdx >= DDSF_SHM_SLOT_COUNT
    ) {
      return;
    }

    const slotBuf = this.slots[slotIdx];
    if (!slotBuf) {
      // Queue latest notification for this slot until `sharedbufferreceived` arrives.
      this.pendingSlotReady[slotIdx] = msg;
      return;
    }

    if (msg.byte_len < DDSF_HEADER_BYTES || msg.byte_len > slotBuf.byteLength) {
      this.staleSlotDrops += 1;
      return;
    }

    const dv = this.slotViews[slotIdx] ?? new DataView(slotBuf);
    const expectedSeq = msg.frame_seq >>> 0;
    const magicPre = dv.getUint32(DdsfHeaderOffsets.magic, true);
    const seqPre = dv.getUint32(DdsfHeaderOffsets.frameSeq, true);
    if (magicPre !== DDSF_MAGIC || seqPre !== expectedSeq) {
      this.staleSlotDrops += 1;
      return;
    }

    // Zero-copy ingest: `slotBuf` is passed directly without `.slice()`.
    const ok = this.topicBus.ingestFrame(slotBuf, 0, msg.byte_len);

    const magicPost = dv.getUint32(DdsfHeaderOffsets.magic, true);
    const seqPost = dv.getUint32(DdsfHeaderOffsets.frameSeq, true);
    if (magicPost !== DDSF_MAGIC || seqPost !== expectedSeq) {
      this.tornSlotDetections += 1;
    }
    if (ok) {
      this.shmFramesIngested += 1;
    }
  }

  private processRawFrame(payload: unknown): void {
    if (!this.receiving) {
      return;
    }
    let ok = false;
    if (payload instanceof ArrayBuffer) {
      ok = this.topicBus.ingestFrame(payload, 0, payload.byteLength);
    } else if (ArrayBuffer.isView(payload)) {
      ok = this.topicBus.ingestFrame(
        payload.buffer as ArrayBuffer,
        payload.byteOffset,
        payload.byteLength,
      );
    } else if (Array.isArray(payload)) {
      const bytes = Uint8Array.from(payload as number[]);
      ok = this.topicBus.ingestFrame(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    }
    if (ok) {
      this.rawFramesIngested += 1;
    }
  }

  private resetSlotState(): void {
    this.releaseHeldBuffers();
    this.pendingSlotReady = createEmptySlotArray();
  }

  private releaseHeldBuffers(): void {
    const webview = this.webview;
    for (let i = 0; i < this.slots.length; i += 1) {
      const buf = this.slots[i];
      if (buf && webview?.releaseBuffer) {
        webview.releaseBuffer(buf);
      }
      this.slots[i] = null;
      this.slotViews[i] = null;
    }
  }

  private cleanupClientResources(): void {
    this.receiving = false;
    this.attached = false;
    this.activeMode = null;

    if (this.webview && this.shmListener) {
      this.webview.removeEventListener("sharedbufferreceived", this.shmListener);
    }
    this.shmListener = null;

    if (this.slotChannel) {
      this.slotChannel.onmessage = null;
      this.slotChannel = null;
    }
    if (this.rawChannel) {
      this.rawChannel.onmessage = null;
      this.rawChannel = null;
    }

    this.releaseHeldBuffers();
    this.pendingSlotReady = createEmptySlotArray();
    this.webview = null;
  }
}
